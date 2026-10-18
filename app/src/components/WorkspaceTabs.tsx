import { useRef, type KeyboardEvent } from "react";

export type Workspace = "edit" | "record";

const WORKSPACES: Workspace[] = ["edit", "record"];

export function WorkspaceTabs({ active, onChange }: {
  active: Workspace;
  onChange: (workspace: Workspace) => void;
}) {
  const buttons = useRef<Partial<Record<Workspace, HTMLButtonElement | null>>>({});

  function handleKeyDown(event: KeyboardEvent<HTMLButtonElement>, workspace: Workspace) {
    const index = WORKSPACES.indexOf(workspace);
    let next: Workspace;
    switch (event.key) {
      case "ArrowRight": next = WORKSPACES[(index + 1) % WORKSPACES.length]; break;
      case "ArrowLeft": next = WORKSPACES[(index + WORKSPACES.length - 1) % WORKSPACES.length]; break;
      case "Home": next = WORKSPACES[0]; break;
      case "End": next = WORKSPACES[WORKSPACES.length - 1]; break;
      default: return;
    }
    event.preventDefault();
    onChange(next);
    buttons.current[next]?.focus();
  }

  return (
    <div className="vfl-workspace-tabs" role="tablist" aria-label="Workspace">
      {WORKSPACES.map((workspace) => (
        <button
          key={workspace}
          ref={(button) => { buttons.current[workspace] = button; }}
          type="button"
          role="tab"
          id={`vfl-workspace-tab-${workspace}`}
          aria-controls={`vfl-workspace-${workspace}`}
          aria-selected={active === workspace}
          tabIndex={active === workspace ? 0 : -1}
          onClick={() => onChange(workspace)}
          onKeyDown={(event) => handleKeyDown(event, workspace)}
        >
          {workspace === "edit" ? (
            <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M3.5 5.5h13m-13 9h13M7 3v5m6 4v5" /></svg>
          ) : (
            <svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="10" cy="10" r="5.5" /><circle cx="10" cy="10" r="2" /></svg>
          )}
          {workspace === "edit" ? "Edit" : "Record"}
        </button>
      ))}
    </div>
  );
}
