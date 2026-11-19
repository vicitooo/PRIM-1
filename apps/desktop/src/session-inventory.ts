import type { SessionSnapshot } from "./types";

/** Local metadata is evidence, never an automatic session binding. */
export interface ConversationCandidate {
  provider: string;
  id: string;
  label: string | null;
  workspace: string;
  metadata_paths: string[];
  updated_at: string | null;
  kind: "root" | "subagent" | "unknown";
  evidence: string;
  conflict: boolean;
  /** Native resume-format limits; absent on older discovery responses. */
  attach_unavailable?: string | null;
}
export interface ProviderDiscovery {
  provider: string;
  entries_examined: number;
  truncated: boolean;
  errors: string[];
}
export interface SessionInventory {
  workspace: string;
  candidates: ConversationCandidate[];
  providers: ProviderDiscovery[];
  truncated: boolean;
}

type ConversationDriver = "claude" | "codex" | "grok";
export type InventoryMember = Pick<SessionSnapshot,
  "session_id" | "label" | "driver" | "running" | "output_draining" | "lifecycle_state" | "working_dir">;
export interface AttachLocalSessionRequest {
  session_id: string;
  driver: ConversationDriver;
  conversation_id: string;
}
export interface InventoryActions {
  members: () => readonly InventoryMember[];
  canAttach: () => boolean;
  confirm: (message: string) => Promise<boolean>;
  attach: (request: AttachLocalSessionRequest) => Promise<void>;
  refresh: () => Promise<unknown>;
}

function candidateDriver(candidate: ConversationCandidate): ConversationDriver | null {
  if (candidate.kind !== "root" || candidate.conflict || candidate.attach_unavailable) return null;
  switch (candidate.provider) {
    case "claude": case "codex": case "grok": return candidate.provider;
    default: return null;
  }
}

/** Match session_inventory::normalize: lexical equality, never a parent/child
 * or filesystem-alias inference. Only trailing separators are trimmed. */
function normalizeWorkspace(path: string): string {
  const windows = path.replace(/\//g, "\\");
  const unprefixed = windows.startsWith("\\\\?\\") ? windows.slice(4) : windows;
  return unprefixed.replace(/\\+$/, "").toLowerCase();
}

/** This is a current UI hint, never a native ownership or admission decision. */
export function attachableMembers(candidate: ConversationCandidate, members: readonly InventoryMember[]): InventoryMember[] {
  const driver = candidateDriver(candidate);
  const workspace = normalizeWorkspace(candidate.workspace);
  return members.filter((member) => member.driver === driver && !member.running
    && !member.output_draining && member.lifecycle_state === "closed"
    && normalizeWorkspace(member.working_dir) === workspace);
}

export function inventorySummary(inventory: SessionInventory): string {
  const count = inventory.candidates.length;
  const partial = inventory.truncated || inventory.providers.some((p) => p.errors.length > 0);
  return `${count} conversation candidate${count === 1 ? "" : "s"} found${partial ? " · partial scan" : ""}. Scanning never attaches or starts a member.`;
}

export function candidateStatus(candidate: ConversationCandidate): string {
  if (candidate.conflict) return "Conflicting metadata — not verified";
  if (candidate.kind === "subagent") return "Subagent — not a standalone conversation";
  if (candidate.kind === "unknown") return "Conversation type unverified";
  if (candidate.attach_unavailable) return `Attachment unavailable — ${candidate.attach_unavailable}`;
  return "Root conversation metadata — not an agent binding";
}

export function installSessionInventory(
  scan: HTMLButtonElement,
  status: HTMLElement,
  results: HTMLElement,
  discover: () => Promise<SessionInventory>,
  actions: InventoryActions,
): void {
  let scanning = false;
  let attaching = false;
  const updateControls = new Set<() => void>();
  const updateAll = (): void => {
    scan.disabled = scanning || attaching;
    for (const update of updateControls) update();
  };

  const addAttachment = (detail: HTMLDetailsElement, candidate: ConversationCandidate): void => {
    const driver = candidateDriver(candidate);
    if (!driver) return;
    const note = document.createElement("p");
    note.className = "view-hint";
    note.textContent = `Choose a stopped member whose folder matches ${candidate.workspace}. `
      + "Attachment stays held for recovery; it does not prove the conversation is free elsewhere.";
    const controls = document.createElement("div");
    controls.className = "local-conversation-attach";
    const label = document.createElement("label");
    label.textContent = "Member";
    const select = document.createElement("select");
    select.dataset.trialMutation = "";
    label.append(select);
    const attach = document.createElement("button");
    attach.type = "button";
    attach.className = "ghost";
    attach.textContent = "Attach for recovery";
    attach.dataset.trialMutation = "";
    const result = document.createElement("p");
    result.className = "task-folder-result";
    result.setAttribute("role", "status");
    result.setAttribute("aria-live", "polite");
    let attachedTo: string | null = null;
    let selectedLabel = "Selected member";

    const selectedMember = (): InventoryMember | undefined =>
      attachableMembers(candidate, actions.members()).find((member) => member.session_id === select.value);
    const update = (): void => {
      const allowed = actions.canAttach();
      select.disabled = scanning || attaching || !allowed;
      attach.disabled = select.disabled || !selectedMember() || attachedTo === select.value;
      const held = "Attachment is disabled while startup is paused or the preview is held.";
      select.title = attach.title = allowed ? "" : held;
    };
    const refreshMembers = (): void => {
      const previous = select.value;
      const members = attachableMembers(candidate, actions.members());
      select.replaceChildren();
      const placeholder = document.createElement("option");
      placeholder.value = "";
      placeholder.textContent = members.length ? "Choose a stopped member…" : "No stopped member of this harness in this folder";
      select.append(placeholder);
      for (const member of members) {
        const option = document.createElement("option");
        option.value = member.session_id;
        option.textContent = `${member.label} · ${member.session_id.slice(0, 8)} · ${member.working_dir}`;
        select.append(option);
      }
      if (previous && !members.some((member) => member.session_id === previous)) {
        const unavailable = document.createElement("option");
        unavailable.value = previous;
        unavailable.textContent = `${selectedLabel} · no longer available`;
        unavailable.disabled = true;
        select.append(unavailable);
      }
      select.value = previous;
      update();
    };
    select.addEventListener("focus", refreshMembers);
    detail.addEventListener("toggle", () => { if (detail.open) refreshMembers(); });
    select.addEventListener("change", () => {
      selectedLabel = selectedMember()?.label ?? selectedLabel;
      update();
    });
    attach.addEventListener("click", async () => {
      if (attaching || scanning || !actions.canAttach() || attachedTo === select.value) return;
      const member = selectedMember();
      if (!member) {
        result.textContent = "Choose an available stopped member of the same harness and folder.";
        refreshMembers();
        return;
      }
      attaching = true;
      updateAll();
      result.dataset.tone = "";
      result.textContent = "Waiting for confirmation…";
      try {
        const message = `Attach conversation ${candidate.id} to ${member.label} (${member.session_id}) for recovery? `
          + "The member’s existing conversation binding may change; its previous recovery references are kept. "
          + "Nothing starts. The conversation stays held until you explicitly choose Resume here on the member.";
        if (await actions.confirm(message) !== true) {
          result.textContent = "Cancelled. No attachment was requested.";
          return;
        }
        // State can change while the native confirmation is open. Native admission
        // still revalidates the exact member/candidate and ownership on submission.
        if (!actions.canAttach() || !attachableMembers(candidate, actions.members()).some((current) => current.session_id === member.session_id)) {
          throw new Error("The member is no longer available or changes are disabled. Nothing was attached.");
        }
        result.textContent = "Attaching for recovery…";
        await actions.attach({ session_id: member.session_id, driver, conversation_id: candidate.id });
        attachedTo = member.session_id;
        const success = `Attached to ${member.label} for recovery. Nothing started. The conversation stays held; use Resume here on the member when ready to hand it over.`;
        result.dataset.tone = "ok";
        result.textContent = success;
        try {
          await actions.refresh();
        } catch (error) {
          result.dataset.tone = "error";
          result.textContent = `${success} State refresh failed: ${String(error)}. Use Refresh state; do not repeat the attachment.`;
        }
      } catch (error) {
        result.dataset.tone = "error";
        result.textContent = `Attachment unavailable: ${String(error)}`;
      } finally {
        attaching = false;
        refreshMembers();
        updateAll();
      }
    });
    updateControls.add(update);
    refreshMembers();
    controls.append(label, attach);
    detail.append(note, controls, result);
  };

  scan.addEventListener("click", async () => {
    if (scan.disabled || scanning || attaching) return;
    scanning = true;
    updateAll();
    status.textContent = "Reading local conversation metadata…";
    try {
      const inventory = await discover();
      // Retain the previous list, disclosure and selection if a new scan fails.
      updateControls.clear();
      results.replaceChildren();
      status.textContent = inventorySummary(inventory);
      const workspace = document.createElement("p");
      workspace.className = "mono";
      workspace.textContent = `Personal repository: ${inventory.workspace}`;
      results.append(workspace);
      for (const provider of inventory.providers) {
        const group = document.createElement("details");
        const heading = document.createElement("summary");
        const candidates = inventory.candidates.filter((c) => c.provider === provider.provider);
        heading.textContent = `${provider.provider} · ${candidates.length}${provider.truncated || provider.errors.length ? " · incomplete" : ""}`;
        group.append(heading);
        for (const error of provider.errors) {
          const note = document.createElement("p");
          note.textContent = error;
          group.append(note);
        }
        if (provider.truncated) {
          const note = document.createElement("p");
          note.textContent = "Scan limit reached. This is not a complete list; omitted conversations may still exist.";
          group.append(note);
        }
        const list = document.createElement("ul");
        for (const candidate of candidates) {
          const item = document.createElement("li");
          const detail = document.createElement("details");
          const title = document.createElement("summary");
          title.textContent = `${candidate.label ?? candidate.id} · ${candidateStatus(candidate)}`;
          detail.append(title);
          for (const value of [candidate.id, candidate.evidence, ...candidate.metadata_paths]) {
            const line = document.createElement("p");
            line.className = "mono";
            line.textContent = value;
            detail.append(line);
          }
          if (candidate.attach_unavailable) {
            const reason = document.createElement("p");
            reason.textContent = candidate.attach_unavailable;
            detail.append(reason);
          }
          if (candidateDriver(candidate)) {
            // A bounded scan can still contain thousands of candidates. Do not
            // multiply every collapsed row by the full member-option inventory.
            let installed = false;
            detail.addEventListener("toggle", () => {
              if (detail.open && !installed) {
                installed = true;
                addAttachment(detail, candidate);
              }
            });
          }
          item.append(detail);
          list.append(item);
        }
        group.append(list);
        results.append(group);
      }
    } catch (error) {
      status.textContent = `Scan unavailable: ${String(error)}. Any previous results are unchanged.`;
    } finally {
      scanning = false;
      updateAll();
    }
  });
}
