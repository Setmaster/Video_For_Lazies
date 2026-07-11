import { useLayoutEffect, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";

type ModalDialogProps = {
  role?: "dialog" | "alertdialog";
  className: string;
  labelledBy?: string;
  describedBy?: string;
  label?: string;
  initialFocus?: "first" | "container";
  onRequestClose?: () => void;
  closeOnBackdrop?: boolean;
  children: ReactNode;
};

const FOCUSABLE_SELECTOR = [
  "button:not([disabled])",
  "a[href]",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

function focusableElements(dialog: HTMLElement) {
  return Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (element) => !element.hidden && element.getAttribute("aria-hidden") !== "true",
  );
}

export function ModalDialog({
  role = "dialog",
  className,
  labelledBy,
  describedBy,
  label,
  initialFocus = "first",
  onRequestClose,
  closeOnBackdrop = false,
  children,
}: ModalDialogProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const requestCloseRef = useRef(onRequestClose);
  const closeOnBackdropRef = useRef(closeOnBackdrop);
  requestCloseRef.current = onRequestClose;
  closeOnBackdropRef.current = closeOnBackdrop;

  useLayoutEffect(() => {
    const dialog = dialogRef.current;
    const root = document.getElementById("root");
    if (!dialog || !root) return;
    const dialogElement = dialog;

    const returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const rootWasInert = root.inert;
    const rootAriaHidden = root.getAttribute("aria-hidden");

    function focusInside() {
      const target = initialFocus === "first" ? focusableElements(dialogElement)[0] ?? dialogElement : dialogElement;
      target.focus({ preventScroll: true });
    }

    function handleKeyDown(event: globalThis.KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        requestCloseRef.current?.();
        return;
      }
      if (event.key !== "Tab") return;

      const focusable = focusableElements(dialogElement);
      if (focusable.length === 0) {
        event.preventDefault();
        dialogElement.focus({ preventScroll: true });
        return;
      }

      const active = document.activeElement;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && (active === first || !dialogElement.contains(active))) {
        event.preventDefault();
        last.focus({ preventScroll: true });
      } else if (!event.shiftKey && (active === last || !dialogElement.contains(active))) {
        event.preventDefault();
        first.focus({ preventScroll: true });
      }
    }

    function handleFocusIn(event: FocusEvent) {
      if (!(event.target instanceof Node) || dialogElement.contains(event.target)) return;
      focusInside();
    }

    document.addEventListener("keydown", handleKeyDown, true);
    document.addEventListener("focusin", handleFocusIn, true);
    focusInside();
    root.inert = true;
    root.setAttribute("aria-hidden", "true");

    return () => {
      document.removeEventListener("keydown", handleKeyDown, true);
      document.removeEventListener("focusin", handleFocusIn, true);
      root.inert = rootWasInert;
      if (rootAriaHidden === null) root.removeAttribute("aria-hidden");
      else root.setAttribute("aria-hidden", rootAriaHidden);
      if (returnFocus?.isConnected) returnFocus.focus({ preventScroll: true });
    };
  }, [initialFocus]);

  useLayoutEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const active = document.activeElement;
    if (active instanceof Node && dialog.contains(active)) return;

    const target = initialFocus === "first" ? focusableElements(dialog)[0] ?? dialog : dialog;
    target.focus({ preventScroll: true });
  });

  return createPortal(
    <div
      className="vfl-modal-backdrop"
      data-modal-backdrop="true"
      role="presentation"
      onClick={(event) => {
        if (event.target === event.currentTarget && closeOnBackdropRef.current) {
          requestCloseRef.current?.();
        }
      }}
    >
      <div
        ref={dialogRef}
        className={className}
        role={role}
        aria-modal="true"
        aria-labelledby={labelledBy}
        aria-describedby={describedBy}
        aria-label={label}
        tabIndex={-1}
      >
        {children}
      </div>
    </div>,
    document.body,
  );
}
