// Developer-mode preference: a persisted flag that reveals the Logs tab.
import { writable } from "svelte/store";

const KEY = "savr.devmode";

export const devMode = writable<boolean>(localStorage.getItem(KEY) === "1");

devMode.subscribe((on) => {
  localStorage.setItem(KEY, on ? "1" : "0");
});
