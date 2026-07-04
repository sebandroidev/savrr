// Minimal toast bus. A writable store keeps this trivially shareable across
// components without prop drilling.
import { writable } from "svelte/store";

export type ToastKind = "info" | "success" | "error";

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
}

export const toasts = writable<Toast[]>([]);

let nextId = 1;

export function pushToast(kind: ToastKind, message: string, ttlMs = 4200) {
  const id = nextId++;
  toasts.update((list) => [...list, { id, kind, message }]);
  if (ttlMs > 0) {
    setTimeout(() => dismissToast(id), ttlMs);
  }
}

export function dismissToast(id: number) {
  toasts.update((list) => list.filter((t) => t.id !== id));
}

export const notify = {
  info: (m: string) => pushToast("info", m),
  success: (m: string) => pushToast("success", m),
  error: (m: string) => pushToast("error", m),
};
