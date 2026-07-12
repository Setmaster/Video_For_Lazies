import { ModalDialog } from "./ModalDialog";

export type UserRecipeDialogState =
  | { kind: "create" }
  | { kind: "edit"; recipeId: string; recipeName: string }
  | { kind: "delete"; recipeId: string; recipeName: string };

type UserRecipeDialogProps = {
  state: UserRecipeDialogState;
  nameDraft: string;
  descriptionDraft: string;
  resetToCurrentSettings: boolean;
  error: string | null;
  onNameDraftChange: (name: string) => void;
  onDescriptionDraftChange: (description: string) => void;
  onResetToCurrentSettingsChange: (enabled: boolean) => void;
  onConfirm: () => void;
  onRequestDelete: () => void;
  onCancel: () => void;
};

export function UserRecipeDialog({
  state,
  nameDraft,
  descriptionDraft,
  resetToCurrentSettings,
  error,
  onNameDraftChange,
  onDescriptionDraftChange,
  onResetToCurrentSettingsChange,
  onConfirm,
  onRequestDelete,
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
          <h2 id="vfl-recipe-dialog-title">Delete {state.recipeName}?</h2>
          <p id="vfl-recipe-dialog-description">This permanently removes the saved recipe from this app.</p>
        </div>
        {error ? <div className="vfl-error" role="alert">{error}</div> : null}
        <div className="vfl-actions vfl-recipe-dialog-actions">
          <button type="button" onClick={onCancel}>Cancel</button>
          <button type="button" className="danger" onClick={onConfirm}>Delete recipe</button>
        </div>
      </ModalDialog>
    );
  }

  const creating = state.kind === "create";
  return (
    <ModalDialog
      className="vfl-recipe-modal"
      labelledBy="vfl-recipe-dialog-title"
      describedBy="vfl-recipe-dialog-description"
      initialFocus="first"
      onRequestClose={onCancel}
    >
      <div className="vfl-recipe-dialog-copy">
        <h2 id="vfl-recipe-dialog-title">{creating ? "Create recipe" : `Edit ${state.recipeName}`}</h2>
        <p id="vfl-recipe-dialog-description">
          {creating ? "The new recipe will use your current settings." : "Change how this recipe appears or replace its saved settings."}
        </p>
      </div>
      <div className="vfl-field">
        <label htmlFor="vfl-user-recipe-name">Name</label>
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
      <div className="vfl-field">
        <label htmlFor="vfl-user-recipe-description">Short description</label>
        <textarea
          id="vfl-user-recipe-description"
          data-smoke-id="user-recipe-description"
          value={descriptionDraft}
          maxLength={160}
          rows={3}
          onChange={(event) => onDescriptionDraftChange(event.currentTarget.value)}
        />
      </div>
      {!creating ? (
        <label className="vfl-check vfl-check-card">
          <input
            type="checkbox"
            checked={resetToCurrentSettings}
            onChange={(event) => onResetToCurrentSettingsChange(event.currentTarget.checked)}
          />
          <span>Replace saved settings with current settings</span>
        </label>
      ) : null}
      {error ? <div className="vfl-error" role="alert">{error}</div> : null}
      <div className="vfl-actions vfl-recipe-dialog-actions">
        {!creating ? (
          <button type="button" className="danger vfl-recipe-delete-action" onClick={onRequestDelete}>
            Delete recipe
          </button>
        ) : null}
        <button type="button" onClick={onCancel}>Cancel</button>
        <button type="button" className="primary" data-smoke-id="user-recipe-confirm" onClick={onConfirm}>
          {creating ? "Create recipe" : "Save changes"}
        </button>
      </div>
    </ModalDialog>
  );
}
