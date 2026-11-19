import type {
  RoomFallbackMemberOverride, RoomFallbackSettings, RoomFallbackSnapshot,
  RoomSnapshot, SessionSnapshot, SetRoomFallbacksRequest,
} from "./types";

export type FallbackMember = Pick<SessionSnapshot, "session_id" | "label" | "driver">;

/** Guests have no session ID here. Lifecycle does not remove saved settings. */
export function fallbackMembers(room: Pick<RoomSnapshot, "member_ids">, sessions: readonly FallbackMember[]): FallbackMember[] {
  const hosted = new Map(sessions.map((member) => [member.session_id, member]));
  return [...new Set(room.member_ids)].flatMap((id) => {
    const member = hosted.get(id);
    return member && ["claude", "codex", "grok"].includes(member.driver) ? [member] : [];
  });
}

function copySettings(settings: RoomFallbackSettings): RoomFallbackSettings {
  return { ...settings, recipient_ids: [...settings.recipient_ids], overrides: settings.overrides.map((entry) => ({ ...entry })) };
}

export function unavailableFallbackTargets(settings: RoomFallbackSettings, members: readonly FallbackMember[]): string[] {
  const available = new Set(members.map((member) => member.session_id));
  return [...new Set([...settings.recipient_ids, ...settings.overrides.map((entry) => entry.session_id)])]
    .filter((id) => !available.has(id));
}

export interface FallbackDraft {
  snapshot: RoomFallbackSnapshot | null;
  settings: RoomFallbackSettings | null;
  revision: number;
  dirty: boolean;
  loading: boolean;
  saving: boolean;
  error: string;
  members: FallbackMember[];
  /** Changes only when input values must be replaced, never on a status update. */
  formVersion: number;
  generation: number;
  edits: number;
  membership: string;
  invalidationQueued: boolean;
}

interface FallbackApi {
  get(roomId: string): Promise<RoomFallbackSnapshot>;
  set(request: SetRoomFallbacksRequest): Promise<RoomFallbackSnapshot>;
  canEdit(): boolean;
}

function errorText(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (error && typeof error === "object" && "message" in error) return String(error.message);
  return String(error);
}

/** Room-owned in-memory drafts. Native revision and membership checks remain authoritative. */
export class RoomFallbackDrafts {
  readonly rooms = new Map<string, FallbackDraft>();
  activeRoomId: string | null = null;

  constructor(private readonly api: FallbackApi, private readonly changed: () => void = () => {}) {}

  bind(room: Pick<RoomSnapshot, "room_id" | "member_ids" | "membership_revision"> | null, sessions: readonly FallbackMember[]): void {
    this.activeRoomId = room?.room_id ?? null;
    if (!room) { this.changed(); return; }
    let state = this.rooms.get(room.room_id);
    if (!state) {
      state = { snapshot: null, settings: null, revision: 0, dirty: false, loading: false, saving: false,
        error: "", members: [], formVersion: 0, generation: 0, edits: 0, membership: "", invalidationQueued: false };
      this.rooms.set(room.room_id, state);
    }
    state.members = fallbackMembers(room, sessions);
    const membership = JSON.stringify([room.membership_revision, state.members.map((member) => member.session_id)]);
    const refresh = membership !== state.membership;
    state.membership = membership;
    this.changed();
    if (refresh) this.invalidate(room.room_id);
  }

  /** Targeted native change, never a polling timer or an arbitrary feed receipt. */
  invalidate(roomId: string): void {
    const state = this.rooms.get(roomId);
    if (!state) return;
    if (state.loading || state.saving) { state.invalidationQueued = true; return; }
    void this.load(roomId);
  }

  private refreshQueued(roomId: string, state: FallbackDraft): void {
    if (state.invalidationQueued) {
      state.invalidationQueued = false;
      this.invalidate(roomId);
    }
  }

  retire(liveIds: ReadonlySet<string>): void {
    for (const id of this.rooms.keys()) if (!liveIds.has(id)) this.rooms.delete(id);
  }

  edit(roomId: string, update: (settings: RoomFallbackSettings) => void): void {
    const state = this.rooms.get(roomId);
    if (!state?.settings || state.saving || !this.api.canEdit()) return;
    update(state.settings);
    state.edits += 1;
    state.dirty = true;
    state.error = "";
    this.changed();
  }

  async load(roomId: string, discard = false): Promise<void> {
    const state = this.rooms.get(roomId);
    if (!state || state.saving) return;
    const generation = ++state.generation;
    const edits = state.edits;
    state.loading = true;
    state.error = "";
    this.changed();
    try {
      const snapshot = await this.api.get(roomId);
      if (this.rooms.get(roomId) !== state || generation !== state.generation) return;
      if (snapshot.room_id !== roomId) throw new Error("Settings response belongs to another room. Reload to retry.");
      state.snapshot = { ...snapshot,
        notice_warning: snapshot.notice_warning ?? (state.snapshot?.revision === snapshot.revision ? state.snapshot.notice_warning : undefined) };
      // A read started before typing (including Discard) cannot erase later edits.
      // Status-only native commits keep the editable nodes mounted, even when clean.
      if (state.edits === edits && (!state.dirty || discard)
        && (state.settings === null || discard || state.revision !== snapshot.revision)) {
        state.settings = copySettings(snapshot.settings);
        state.revision = snapshot.revision;
        state.dirty = false;
        state.formVersion += 1;
      }
    } catch (error) {
      if (this.rooms.get(roomId) === state && generation === state.generation) state.error = errorText(error);
    } finally {
      if (this.rooms.get(roomId) === state && generation === state.generation) {
        state.loading = false;
        this.changed();
        this.refreshQueued(roomId, state);
      }
    }
  }

  async save(roomId: string): Promise<void> {
    const state = this.rooms.get(roomId);
    if (!state?.settings || state.saving || !state.dirty || !this.api.canEdit()) return;
    if (unavailableFallbackTargets(state.settings, state.members).length) {
      state.error = "A configured member is no longer eligible in this room. Remove its settings below or discard edits and reload.";
      this.changed();
      return;
    }
    const generation = ++state.generation;
    const request = { room_id: roomId, revision: state.revision, settings: copySettings(state.settings) };
    state.loading = false;
    state.saving = true;
    state.error = "";
    this.changed();
    try {
      const snapshot = await this.api.set(request);
      if (this.rooms.get(roomId) !== state || generation !== state.generation) return;
      if (snapshot.room_id !== roomId) throw new Error("Save response belongs to another room. Reload to check the saved settings.");
      state.snapshot = snapshot;
      state.settings = copySettings(snapshot.settings);
      state.revision = snapshot.revision;
      state.dirty = false;
      state.formVersion += 1;
    } catch (error) {
      if (this.rooms.get(roomId) === state && generation === state.generation) state.error = errorText(error);
    } finally {
      if (this.rooms.get(roomId) === state && generation === state.generation) {
        state.saving = false;
        this.changed();
        this.refreshQueued(roomId, state);
      }
    }
  }
}

function element<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  return node;
}

function select(options: readonly (readonly [string, string])[]): HTMLSelectElement {
  const node = element("select");
  for (const [value, label] of options) { const option = element("option", label); option.value = value; node.append(option); }
  return node;
}

function field(title: string, input: HTMLElement): HTMLLabelElement {
  const label = element("label");
  label.append(element("span", title), input);
  return label;
}

const intervals = [["15", "15 min"], ["30", "30 min"], ["60", "1 h"]] as const;

function when(value: string | null | undefined): string {
  if (!value) return "Not scheduled";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

/** v3 W6 (J10): the saved end, a UTC instant, as the person's local
    `datetime-local` value (to the minute); none is empty. */
export function untilInputValue(until: string | null | undefined): string {
  const date = until ? new Date(until) : null;
  if (!date || Number.isNaN(date.getTime())) return "";
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** The local time typed in, as the UTC instant saved; empty means no end. */
export function untilFromInput(value: string): string | null {
  if (!value.trim()) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

function override(settings: RoomFallbackSettings, id: string): RoomFallbackMemberOverride {
  let entry = settings.overrides.find((candidate) => candidate.session_id === id);
  if (!entry) {
    entry = { session_id: id, mode: "inherit", interval_minutes: null, message: null, brief: null };
    settings.overrides.push(entry);
  }
  return entry;
}

/** A separate mount from live member cards; same-room renders never replace editing nodes. */
export function installRoomFallbacks(details: HTMLDetailsElement, summary: HTMLElement, host: HTMLElement, api: FallbackApi): RoomFallbackDrafts {
  let renderedRoom: string | null = null;
  let renderedVersion = -1;
  let updateForm: ((state: FallbackDraft) => void) | null = null;
  const drafts = new RoomFallbackDrafts(api, render);
  details.addEventListener("toggle", () => {
    if (details.open && drafts.activeRoomId) void drafts.load(drafts.activeRoomId);
  });

  function render(): void {
    const id = drafts.activeRoomId;
    const state = id ? drafts.rooms.get(id) : undefined;
    const saved = state?.snapshot?.settings;
    const count = saved && state ? state.members.filter((member) =>
      (saved.recipient_mode === "all" || saved.recipient_ids.includes(member.session_id)) &&
      saved.overrides.find((entry) => entry.session_id === member.session_id)?.mode !== "off").length : 0;
    summary.textContent = !saved ? "Not loaded" : `${saved.enabled ? `${saved.interval_minutes === 60 ? "1 h" : `${saved.interval_minutes} min`} · ${count} agents` : "Off"}${state?.dirty ? " · unsaved" : ""}`;
    if (!id || !state) { host.replaceChildren(); renderedRoom = null; updateForm = null; return; }
    if (!state.settings) {
      host.replaceChildren(element("p", state.error || "Loading fallback settings…"));
      if (state.error) {
        const retry = element("button", "Retry"); retry.type = "button";
        retry.addEventListener("click", () => void drafts.load(id)); host.append(retry);
      }
      renderedRoom = null;
      return;
    }
    if (id !== renderedRoom || state.formVersion !== renderedVersion) {
      renderedRoom = id;
      renderedVersion = state.formVersion;
      updateForm = buildForm(id, state);
    }
    updateForm?.(state);
  }

  function buildForm(roomId: string, initial: FallbackDraft): (state: FallbackDraft) => void {
    const settings = initial.settings!;
    const controls: (HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement | HTMLButtonElement)[] = [];
    const mutation = <T extends typeof controls[number]>(node: T): T => {
      node.dataset.trialMutation = ""; controls.push(node); return node;
    };
    const edit = (update: (value: RoomFallbackSettings) => void) => drafts.edit(roomId, update);
    const enabled = mutation(element("input")); enabled.type = "checkbox"; enabled.checked = settings.enabled;
    enabled.addEventListener("change", () => edit((value) => { value.enabled = enabled.checked; }));
    const cadence = mutation(select(intervals)); cadence.value = String(settings.interval_minutes);
    cadence.addEventListener("change", () => edit((value) => { value.interval_minutes = Number(cadence.value); }));
    // v3 W6 (J10): no reminder at or after this time; PRIM-1 refuses a past one.
    const until = mutation(element("input")); until.type = "datetime-local"; until.value = untilInputValue(settings.until);
    until.addEventListener("change", () => edit((value) => { value.until = untilFromInput(until.value); }));
    const recipients = mutation(select([["all", "All hosted agents"], ["selected", "Selected hosted agents"]])); recipients.value = settings.recipient_mode;
    recipients.addEventListener("change", () => edit((value) => { value.recipient_mode = recipients.value as "all" | "selected"; }));
    const message = mutation(element("textarea")); message.rows = 3; message.value = settings.message;
    message.addEventListener("input", () => edit((value) => { value.message = message.value; }));
    const memberList = element("div"); memberList.className = "room-fallback-members";
    const unavailable = element("div"); unavailable.className = "room-fallback-unavailable";
    const status = element("p"); status.className = "room-fallback-status"; status.setAttribute("aria-live", "polite");
    const next = element("p"); next.className = "room-fallback-help";
    const save = mutation(element("button", "Save")); save.type = "button";
    save.addEventListener("click", () => void drafts.save(roomId));
    const discard = element("button", "Discard edits"); discard.type = "button"; discard.className = "ghost";
    discard.addEventListener("click", () => void drafts.load(roomId, drafts.rooms.get(roomId)?.dirty === true));
    const actions = element("div"); actions.className = "room-fallback-actions"; actions.append(save, discard);
    const help = element("p", "PRIM-1 must be running. First prompt follows the interval; stopped or sleeping agents stay stopped. Pausing cannot retract an already pending or written message.");
    help.className = "room-fallback-help";
    host.replaceChildren(field("Enable fallback messages", enabled), field("Room interval", cadence),
      field("Stop after (your local time; empty for no end)", until), field("Recipients", recipients), field("Room message", message), memberList, unavailable, actions, status, next, help);
    const rows = new Map<string, { node: HTMLDetailsElement; update: (state: FallbackDraft, member: FallbackMember) => void }>();
    let orphanSignature = "";
    return (state) => {
      for (const control of controls) control.disabled = state.saving || !api.canEdit();
      save.disabled = state.saving || state.loading || !state.dirty || !api.canEdit();
      discard.disabled = state.saving || state.loading;
      discard.textContent = state.dirty ? "Discard edits" : "Refresh";
      status.textContent = state.error ? `${state.error}${state.dirty ? " Your edits are kept." : ""}` : state.saving ? "Saving…" : state.loading ? "Refreshing…" : state.dirty ? "Unsaved edits" : `Saved${state.snapshot?.notice_warning ? ` · ${state.snapshot.notice_warning}` : ""}`;
      status.dataset.level = state.error || state.snapshot?.notice_warning ? "error" : "info";
      const due = state.snapshot?.members.map((entry) => entry.next_due_at).filter((value): value is string => value !== null).sort()[0];
      const ends = state.snapshot?.settings.until;
      next.textContent = `Saved schedule · Next: ${when(due)} · Ends: ${ends ? when(ends) : "no end"}`;
      const live = new Set(state.members.map((member) => member.session_id));
      for (const [id, row] of rows) if (!live.has(id)) { row.node.remove(); rows.delete(id); }
      for (const member of state.members) {
        let row = rows.get(member.session_id);
        if (!row) { row = memberRow(member); rows.set(member.session_id, row); memberList.append(row.node); }
        row.update(state, member);
      }
      const missing = unavailableFallbackTargets(state.settings!, state.members);
      const signature = JSON.stringify(missing);
      if (signature !== orphanSignature) {
        orphanSignature = signature;
        unavailable.replaceChildren();
        for (const id of missing) {
          const line = element("div");
          const remove = mutation(element("button", "Remove unavailable member settings")); remove.type = "button";
          remove.addEventListener("click", () => edit((value) => {
            value.recipient_ids = value.recipient_ids.filter((target) => target !== id);
            value.overrides = value.overrides.filter((entry) => entry.session_id !== id);
          }));
          line.append(element("p", `No longer eligible: ${id}`), remove); unavailable.append(line);
        }
      }
      // New controls created by membership refresh must also obey trial admission.
      for (let index = controls.length - 1; index >= 0; index -= 1) {
        if (!host.contains(controls[index])) controls.splice(index, 1);
      }
      for (const control of controls) if (control !== save) control.disabled = state.saving || !api.canEdit();
    };

    function memberRow(member: FallbackMember): { node: HTMLDetailsElement; update: (state: FallbackDraft, member: FallbackMember) => void } {
      const id = member.session_id;
      const entry = settings.overrides.find((value) => value.session_id === id);
      const node = element("details"); node.className = "room-fallback-member";
      const title = element("summary"); node.append(title);
      const include = mutation(element("input")); include.type = "checkbox"; include.checked = settings.recipient_ids.includes(id);
      const includeField = field("Include this member", include);
      include.addEventListener("change", () => edit((value) => {
        value.recipient_ids = value.recipient_ids.filter((target) => target !== id);
        if (include.checked) value.recipient_ids.push(id);
      }));
      const mode = mutation(select([["inherit", "Room default"], ["custom", "Custom"], ["off", "Off"]])); mode.value = entry?.mode ?? "inherit";
      mode.addEventListener("change", () => edit((value) => {
        const choice = override(value, id); choice.mode = mode.value as RoomFallbackMemberOverride["mode"];
        if (choice.mode === "custom") { choice.interval_minutes ??= value.interval_minutes; choice.message ??= value.message; }
        customCadence.value = String(choice.interval_minutes ?? value.interval_minutes); customMessage.value = choice.message ?? value.message;
      }));
      const customCadence = mutation(select(intervals)); customCadence.value = String(entry?.interval_minutes ?? settings.interval_minutes);
      customCadence.addEventListener("change", () => edit((value) => { override(value, id).interval_minutes = Number(customCadence.value); }));
      const customMessage = mutation(element("textarea")); customMessage.rows = 3; customMessage.value = entry?.message ?? settings.message;
      customMessage.addEventListener("input", () => edit((value) => { override(value, id).message = customMessage.value; }));
      const custom = element("div"); custom.className = "room-fallback-custom";
      custom.append(field("Member interval", customCadence), field("Replacement message", customMessage));
      const brief = mutation(element("textarea")); brief.rows = 2; brief.value = entry?.brief ?? "";
      brief.addEventListener("input", () => edit((value) => { override(value, id).brief = brief.value === "" ? null : brief.value; }));
      const outcome = element("p"); outcome.className = "room-fallback-help";
      node.append(includeField, field("Schedule", mode), custom, field("Appended brief (optional, literal text)", brief), outcome);
      return { node, update(state, current) {
        const choice = state.settings!.overrides.find((value) => value.session_id === id);
        const modeLabel = choice?.mode === "custom" ? "Custom" : choice?.mode === "off" ? "Off" : "Room default";
        title.textContent = `${current.label} · ${modeLabel}`;
        title.title = `${current.driver} · ${id}`;
        includeField.hidden = state.settings!.recipient_mode !== "selected";
        custom.hidden = choice?.mode !== "custom";
        const receipt = state.snapshot?.members.find((value) => value.session_id === id);
        outcome.textContent = `Next: ${when(receipt?.next_due_at)}${receipt?.last_status ? ` · Last: ${receipt.last_status}` : ""}${receipt?.note ? ` · ${receipt.note}` : ""}${receipt?.last_message_id ? ` · Message: ${receipt.last_message_id}` : ""}. Written or fetched does not mean answered.`;
      } };
    }
  }
  return drafts;
}
