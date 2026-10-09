import { useState, type FormEvent } from "react";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../store";

/**
 * Choose the project new shells start in (phase1-plan Slice D), and register one.
 * The folder is typed, not picked: the window has no file-system access by design
 * (ARCHITECTURE §6.5); the daemon checks that it exists.
 */
export function ProjectPicker() {
  const { projects, selected, connected, select, add } = useStore(
    useShallow((s) => ({
      projects: s.projects,
      selected: s.selectedProject,
      connected: s.daemon.connected,
      select: s.selectProject,
      add: s.addProject,
    })),
  );
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState("");
  const [path, setPath] = useState("");

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    if (await add(name.trim(), path.trim())) {
      setAdding(false);
      setName("");
      setPath("");
    }
  };

  return (
    <section aria-labelledby="project-heading" className="projects">
      <h2 id="project-heading" className="eyebrow">Project</h2>
      <div className="project-row">
        <select
          className="project-select"
          aria-label="Project new shells start in"
          data-testid="project-select"
          value={selected ?? ""}
          disabled={!connected}
          onChange={(e) => select(e.target.value || null)}
        >
          <option value="">No project (profile folder)</option>
          {projects.map((p) => (
            <option key={p.name} value={p.name} title={p.path}>
              {p.name}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="btn btn-quiet"
          aria-expanded={adding}
          disabled={!connected}
          onClick={() => setAdding((a) => !a)}
          data-testid="project-add-toggle"
        >
          {adding ? "Cancel" : "Add"}
        </button>
      </div>
      {selected && <p className="project-path mono dim">{projects.find((p) => p.name === selected)?.path}</p>}
      {adding && (
        <form className="project-form" onSubmit={(e) => void handleSubmit(e)}>
          <label>
            Name
            <input value={name} onChange={(e) => setName(e.target.value)} placeholder="web-app" data-testid="project-name" required />
          </label>
          <label>
            Folder
            <input value={path} onChange={(e) => setPath(e.target.value)} placeholder={"C:\\src\\web-app"} data-testid="project-path" required />
          </label>
          <button type="submit" className="btn" data-testid="project-add">
            Register
          </button>
        </form>
      )}
    </section>
  );
}
