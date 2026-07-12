import { ModalDialog } from "./ModalDialog";

export type UserRecipeDialogState =
  | { kind: "save" }
  | { kind: "rename"; recipeId: string; recipeName: string }
  | { kind: "delete"; recipeId: string; recipeName: string };

type UserRecipeDialogProps = {
  state: UserRecipeDialogState;
  nameDraft: string;
  currentSettingsSummary: string;
  error: string | null;
  onNameDraftChange: (name: string) => void;
  onConfirm: () => void;
  onCancel: () => void;
};

export function UserRecipeDialog({
  state,
  nameDraft,
  currentSettingsSummary,
  error,
  onNameDraftChange,
  onConfirm,
  onCancel,
}: UserRecipeDialogProps) {
  if (state.kind === "delete") {
    return (
      <ModalDialog
        role="alertdialog"
        className="vfl-recipe-modal"
        labelledBy="vfl-recipe-dialog-title"
        describedBy="vfl-recipe-dialog-description"
        initialFocus="first"
        onRequestClose={onCancel}
      >
        <div className="vfl-recipe-dialog-copy">
          <div className="vfl-about-kicker">Your recipes</div>
          <h2 id="vfl-recipe-dialog-title">Delete {state.recipeName}?</h2>
          <p id="vfl-recipe-dialog-description">
            This removes the local recipe from this app. It does not change any files or the current workbench.
          </p>
        </div>
        <div className="vfl-actions vfl-recipe-dialog-actions">
          <button type="button" onClick={onCancel}>Cancel</button>
          <button type="button" className="danger" onClick={onConfirm}>Delete recipe</button>
        </div>
      </ModalDialog>
    );
  }

  const saving = state.kind === "save";
  return (
    <ModalDialog
      className="vfl-recipe-modal"
      labelledBy="vfl-recipe-dialog-title"
      describedBy={
        saving
          ? "vfl-recipe-dialog-description vfl-recipe-values-summary vfl-recipe-privacy-summary"
          : "vfl-recipe-dialog-description"
      }
      initialFocus="first"
      onRequestClose={onCancel}
    >
      <div className="vfl-recipe-dialog-copy">
        <div className="vfl-about-kicker">Your recipes</div>
        <h2 id="vfl-recipe-dialog-title">{saving ? "Save current settings" : `Rename ${state.recipeName}`}</h2>
        <p id="vfl-recipe-dialog-description">
          {saving
            ? "Save a reusable output plan in this app's local storage."
            : "Choose a new local name. The saved settings do not change."}
        </p>
      </div>
      <div className="vfl-field">
        <label htmlFor="vfl-user-recipe-name">Recipe name</label>
        <input
          id="vfl-user-recipe-name"
          data-smoke-id="user-recipe-name"
          value={nameDraft}
          maxLength={64}
          autoComplete="off"
          onChange={(event) => onNameDraftChange(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key !== "Enter") return;
            event.preventDefault();
            onConfirm();
          }}
        />
      </div>
      {saving ? (
        <div className="vfl-recipe-privacy">
          <div id="vfl-recipe-values-summary">
            <strong>Current values:</strong> {currentSettingsSummary}
          </div>
          <div id="vfl-recipe-privacy-summary">
            <strong>Saved fields:</strong> format, size target, resize, audio, uniqueness, and encoder settings.
            <br />
            <strong>Never saved:</strong> media or output paths, title, trim, crop, transforms, color edits, HDR conversion choice,
            diagnostics, or queue and job state. Metadata privacy remains a separate global setting.
          </div>
        </div>
      ) : null}
      {error ? <div className="vfl-error" role="alert">{error}</div> : null}
      <div className="vfl-actions vfl-recipe-dialog-actions">
        <button type="button" className="primary" data-smoke-id="user-recipe-confirm" onClick={onConfirm}>
          {saving ? "Save recipe" : "Rename recipe"}
        </button>
        <button type="button" onClick={onCancel}>Cancel</button>
      </div>
    </ModalDialog>
  );
}
