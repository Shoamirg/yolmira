//! Token facts (user SID, elevation, integrity), security descriptors from SDDL, and
//! the identity of another process (used to verify the pipe server).

use super::{from_wide, last_error, wide, OwnedHandle};
use std::ffi::c_void;
use std::io;
use std::path::Path;
use windows_sys::Win32::Foundation::{LocalFree, GENERIC_WRITE, HANDLE, HLOCAL};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    SE_OBJECT_TYPE,
};
use windows_sys::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenElevation,
    TokenIntegrityLevel, TokenUser, DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Mandatory integrity RIDs (`SECURITY_MANDATORY_*_RID`).
pub mod integrity {
    pub const LOW: u32 = 0x1000;
    pub const MEDIUM: u32 = 0x2000;
    pub const HIGH: u32 = 0x3000;
}

/// Who owns a token: user SID and integrity RID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub sid: String,
    pub integrity: u32,
}

fn open_token(process: HANDLE) -> io::Result<OwnedHandle> {
    let mut token = std::ptr::null_mut();
    // SAFETY: `process` is a valid process handle; `token` is an out-param.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    OwnedHandle::new(token).ok_or_else(|| io::Error::other("OpenProcessToken returned no handle"))
}

/// Variable-size token information in a u64-aligned buffer.
fn token_info(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
    let mut len = 0u32;
    // SAFETY: size query with a null buffer; sets `len`.
    unsafe { GetTokenInformation(token.raw(), class, std::ptr::null_mut(), 0, &mut len) };
    let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
    // SAFETY: `buf` holds at least `len` bytes.
    let ok =
        unsafe { GetTokenInformation(token.raw(), class, buf.as_mut_ptr().cast(), len, &mut len) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sid` is valid for the call; `out` receives a LocalAlloc'd string.
    if unsafe { ConvertSidToStringSidW(sid, &mut out) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `out` is NUL-terminated; freed once after copying.
    Ok(unsafe { take_local_wide(out) })
}

/// Copy a LocalAlloc'd NUL-terminated wide string and free it.
unsafe fn take_local_wide(p: *mut u16) -> String {
    // SAFETY (caller): `p` is a valid NUL-terminated LocalAlloc'd wide string.
    unsafe {
        let n = (0..).take_while(|&i| *p.add(i) != 0).count();
        let s = from_wide(std::slice::from_raw_parts(p, n));
        LocalFree(p as HLOCAL);
        s
    }
}

fn token_identity(token: &OwnedHandle) -> io::Result<Identity> {
    let user = token_info(token, TokenUser)?;
    // SAFETY: a successful TokenUser query starts with TOKEN_USER; its SID points into `user`.
    let sid = sid_string(unsafe { (*(user.as_ptr() as *const TOKEN_USER)).User.Sid })?;
    let label = token_info(token, TokenIntegrityLevel)?;
    // SAFETY: TOKEN_MANDATORY_LABEL header; the label SID's last sub-authority is the RID.
    let integrity = unsafe {
        let sid = (*(label.as_ptr() as *const TOKEN_MANDATORY_LABEL))
            .Label
            .Sid;
        let count = *GetSidSubAuthorityCount(sid);
        *GetSidSubAuthority(sid, u32::from(count.saturating_sub(1)))
    };
    Ok(Identity { sid, integrity })
}

/// This process's user SID and integrity level.
pub fn current_identity() -> io::Result<Identity> {
    // SAFETY: the pseudo-handle is always valid.
    token_identity(&open_token(unsafe { GetCurrentProcess() })?)
}

/// The current user's SID as a string (`S-1-5-21-…`).
pub fn current_user_sid() -> io::Result<String> {
    current_identity().map(|i| i.sid)
}

/// Identity of process `pid` (needs PROCESS_QUERY_LIMITED_INFORMATION on it).
pub fn process_identity(pid: u32) -> io::Result<Identity> {
    // SAFETY: plain open; the handle is owned below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    let process = OwnedHandle::new(h).ok_or_else(io::Error::last_os_error)?;
    token_identity(&open_token(process.raw())?)
}

/// Whether this process runs with an elevated (Administrator) token (ARCHITECTURE §6.5).
pub fn is_elevated() -> io::Result<bool> {
    // SAFETY: the pseudo-handle is always valid.
    let token = open_token(unsafe { GetCurrentProcess() })?;
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    // SAFETY: `elevation` is a correctly sized out-buffer.
    let ok = unsafe {
        GetTokenInformation(
            token.raw(),
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(elevation.TokenIsElevated != 0)
}

/// Full file access (`FA`: named pipes and token files are both file objects) for one
/// SID only, protected from inheritance, with a mandatory label
/// that denies lower integrity levels both read and write (`NWNR`). `elevated` labels
/// the object High, so an unelevated process of the same user can't use it.
pub fn owner_only_sddl(sid: &str, elevated: bool) -> String {
    let level = if elevated { "HI" } else { "ME" };
    format!("D:P(A;;FA;;;{sid})S:(ML;;NWNR;;;{level})")
}

/// A security descriptor parsed from SDDL, freed on drop.
pub struct SecurityDescriptor {
    sd: PSECURITY_DESCRIPTOR,
    attrs: SECURITY_ATTRIBUTES,
}

// SAFETY: the descriptor is immutable after creation and only read by the kernel.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let text = wide(sddl);
        // SAFETY: `text` is NUL-terminated; `sd` receives a LocalAlloc'd descriptor.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        Ok(Self { sd, attrs })
    }

    /// Pointer for APIs taking `LPSECURITY_ATTRIBUTES`. Valid while `self` lives.
    pub fn attributes(&self) -> *const SECURITY_ATTRIBUTES {
        &self.attrs
    }

    pub fn attributes_mut_void(&mut self) -> *mut c_void {
        (&mut self.attrs as *mut SECURITY_ATTRIBUTES).cast()
    }

    /// This descriptor's DACL and label as Windows renders them (well-known SIDs become
    /// aliases such as `LA`), for comparing with `object_sddl`.
    pub fn to_sddl(&self) -> io::Result<String> {
        sd_to_sddl(self.sd)
    }
}

/// The ACE strings of an SDDL (`(A;;FA;;;S-1-…)`, `(ML;;NWNR;;;ME)`), sorted: comparable
/// regardless of header flags such as `AI`.
pub fn sddl_aces(sddl: &str) -> Vec<String> {
    let mut aces: Vec<String> = sddl
        .split('(')
        .skip(1)
        .filter_map(|rest| rest.split_once(')').map(|(ace, _)| format!("({ace})")))
        .collect();
    aces.sort();
    aces
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: `sd` came from LocalAlloc in ConvertStringSecurityDescriptor…; freed once.
        unsafe { LocalFree(self.sd as HLOCAL) };
    }
}

/// SDDL of an object's DACL and mandatory label (handle needs READ_CONTROL).
pub fn object_sddl(
    object: &impl std::os::windows::io::AsRawHandle,
    kind: SE_OBJECT_TYPE,
) -> io::Result<String> {
    let handle: HANDLE = object.as_raw_handle();
    let info = DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    use std::ptr::null_mut;
    // SAFETY: valid handle; unused out-params are null; `sd` receives a LocalAlloc'd descriptor.
    let rc = unsafe {
        GetSecurityInfo(
            handle,
            kind,
            info,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        )
    };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let result = sd_to_sddl(sd);
    // SAFETY: freed once.
    unsafe { LocalFree(sd as HLOCAL) };
    result
}

fn sd_to_sddl(sd: PSECURITY_DESCRIPTOR) -> io::Result<String> {
    let info = DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sd` is valid; `text` receives a LocalAlloc'd string.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            info,
            &mut text,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `text` is a NUL-terminated LocalAlloc'd string.
    Ok(unsafe { take_local_wide(text) })
}

/// Write `path` as a new file whose security descriptor is `owner_only_sddl(sid, elevated)`.
/// An existing file (whatever its ACL) or link is removed first: `CREATE_ALWAYS` would
/// keep an existing file's ACL.
pub fn write_private_file(path: &Path, sid: &str, elevated: bool, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::windows::io::FromRawHandle;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let sd = SecurityDescriptor::from_sddl(&owner_only_sddl(sid, elevated))?;
    let name = wide(path);
    // SAFETY: `name` is NUL-terminated; `sd` outlives the call.
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            0,
            sd.attributes(),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    let h = OwnedHandle::new(h).ok_or_else(|| io::Error::from_raw_os_error(last_error() as i32))?;
    // SAFETY: ownership moves to `File`, which closes it exactly once.
    let mut file = unsafe { std::fs::File::from_raw_handle(h.into_raw()) };
    file.write_all(data)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;

    #[test]
    fn identity_is_this_user_at_medium_or_high() {
        let me = current_identity().unwrap();
        assert!(me.sid.starts_with("S-1-5-"), "{me:?}");
        assert!(me.integrity >= integrity::MEDIUM, "{me:?}");
        assert_eq!(process_identity(std::process::id()).unwrap(), me);
    }

    #[test]
    fn elevation_query_succeeds() {
        assert!(is_elevated().is_ok());
    }

    #[test]
    fn private_file_gets_owner_dacl_and_label_even_when_replacing_a_permissive_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, b"old, inherited ACL").unwrap();
        let sid = current_user_sid().unwrap();
        write_private_file(&path, &sid, false, b"xyz").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"xyz");
        let f = std::fs::File::open(&path).unwrap();
        let sddl = object_sddl(&f, SE_FILE_OBJECT).unwrap();
        let expected = SecurityDescriptor::from_sddl(&owner_only_sddl(&sid, false))
            .and_then(|d| d.to_sddl())
            .unwrap();
        assert!(sddl.starts_with("D:P"), "{sddl}");
        assert_eq!(
            sddl_aces(&sddl),
            sddl_aces(&expected),
            "{sddl} vs {expected}"
        );
        assert!(sddl.contains("(ML;;NWNR;;;ME)"), "{sddl}");
    }

    #[test]
    fn sddl_aces_ignores_header_flags() {
        assert_eq!(
            sddl_aces("D:P(A;;FA;;;LA)S:AI(ML;;NWNR;;;ME)"),
            sddl_aces("D:P(A;;FA;;;LA)S:(ML;;NWNR;;;ME)")
        );
    }

    #[test]
    fn bad_sddl_is_an_error() {
        assert!(SecurityDescriptor::from_sddl("not sddl").is_err());
    }
}
