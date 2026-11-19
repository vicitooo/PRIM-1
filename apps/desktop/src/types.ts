export type DriverKind = "claude" | "codex" | "grok" | "prime" | "generic_terminal";
export type PermissionProfile = "normal" | "unsafe";
/** v3 W4: how PRIM-1 connects a Codex member; fixed when it is created. */
export type CodexTransport = "pty" | "app_server";
/** v3 W4 S5: whether PRIM-1 follows a running App Server member's thread
    (connection health only, never a delivery promise). */
export type AppServerFollow = "waiting_for_thread" | "followed" | "reconnecting" | "not_followed";
export type LifecycleState =
  | "starting"
  | "ready"
  | "busy"
  | "idle"
  | "stalled"
  | "restarting"
  | "failed"
  | "closed";
export type SessionErrorKind = "executable_not_found";
export type MessageScope = "direct" | "room" | "system" | "private";
export type LogLevel = "info" | "warn" | "error";
export type WorkState =
  | "idle"
  | "thinking"
  | "tool_call"
  | "blocked"
  | "error_loop"
  | "exited";
export type AlertSeverity = "info" | "warn" | "critical";
export type SupervisorAlertType =
  | "session_stall_detected"
  | "operator_attention";
export type RouteDeliveryPhase = "resolved" | "written" | "failed";
export type SessionExitReason =
  | "clean_exit"
  | "crash_exit"
  | "operator_stop"
  | "restart_stop"
  | "pty_error"
  | "process_disappeared";

export interface RunEventIdentity {
  session_id: string;
  run_id: string;
  generation: number;
  sequence: number;
}

export interface SessionSnapshot {
  session_id: string;
  alias: string;
  label: string;
  driver: DriverKind;
  permission_profile: PermissionProfile;
  /** Saved requested launch choices; not observed effective settings. */
  launch_choices: SessionLaunchChoices;
  /** Why the next Launch cannot resume the stored conversation (S5). */
  recovery: SessionRecovery | null;
  /** Superseded references kept when a fresh conversation replaced them. */
  recovery_history: SessionRecovery[];
  lifecycle_state: LifecycleState;
  working_dir: string;
  generation: number;
  run_id: string | null;
  run_event_sequence: number;
  /** Authoritative [columns, rows]; absent only in a legacy backend snapshot. */
  pty_size?: [number, number] | null;
  process_id: number | null;
  running: boolean;
  output_draining?: boolean;
  /** Native cancellable crash-restart backoff; absent in older snapshots. Stop cancels it. */
  crash_respawn_waiting?: boolean;
  /** Native idle hibernation; absent on older snapshots. Stop revokes automatic wake. */
  hibernated?: boolean;
  /** A harness conversation id is stored: the next Launch resumes it. */
  resume_available: boolean;
  /** Had a live run when the app last went down (workspace restore target). */
  was_running_at_shutdown: boolean;
  last_activity_at: string | null;
  last_error: string | null;
  last_error_kind: SessionErrorKind | null;
  /** Operator grants beyond the member's room (A109); absent in a legacy backend snapshot. */
  permissions?: MemberPermissions;
  /** Set while the running member waits on a person (a startup trust
      question, an approval, an error loop): what PRIM-1 saw and what clears
      it. Its lifecycle state alone reads ready or idle meanwhile. */
  blocked_reason?: string | null;
  /** The running member's observed work state (A16 D5): its lifecycle reads
      ready while it thinks or runs a tool. Absent before the run's first
      observation and when stopped; session_work_state events keep it current. */
  work_state?: WorkState | null;
  /** v3 W2: busy only because the member's own record (its Codex rollout)
      shows a turn still open while its screen reads quiet. */
  work_state_from_record?: boolean;
  /** v3 W4: omitted while "pty". */
  codex_transport?: CodexTransport;
  /** v3 W4 S5: set only while an App Server member runs. */
  app_server?: AppServerFollow | null;
}

/** roam: list, peek, join and leave rooms, deliver across rooms.
    maintain: request a PRIM-1 upgrade (upgrade_request, A108).
    manage: create, start, stop, restart, brief and remove room members (A110);
    omitted by the backend while off. */
export interface MemberPermissions {
  roam: boolean;
  maintain: boolean;
  manage?: boolean;
}

export interface RoomFeedCursor {
  epoch: string;
  sequence: number;
}

export interface RoomSnapshot {
  room_id: string;
  label: string;
  /** Explicit operator choice; visiting a room never changes it. */
  resume_on_restart: boolean;
  member_ids: string[];
  membership_revision: number;
  feed_epoch: string;
  feed_oldest_sequence: number;
  feed_next_sequence: number;
  /** An unusable room log blocks feed operations, while member terminals remain available. */
  recovery_error?: string | null;
  history_replay_pending?: boolean;
  writer_recovery?: { kind: "transient" | "structural"; reason: string } | null;
  /** Earlier legacy history was unavailable; the new history remains usable. */
  history_restarted?: boolean;
  /** Outside agents invited into this room; never members. */
  guests?: RoomGuest[];
}

export interface RoomFallbackSettings {
  enabled: boolean;
  interval_minutes: number;
  message: string;
  recipient_mode: "all" | "selected";
  recipient_ids: string[];
  overrides: RoomFallbackMemberOverride[];
  /** v3 W6 (J10): no reminder at or after this UTC time; absent means no end. */
  until?: string | null;
}

export interface RoomFallbackMemberOverride {
  session_id: string;
  mode: "inherit" | "custom" | "off";
  interval_minutes: number | null;
  message: string | null;
  brief: string | null;
}

export interface RoomFallbackMemberStatus {
  session_id: string;
  next_due_at: string | null;
  last_message_id: string | null;
  last_status: string | null;
  note: string | null;
}

export interface RoomFallbackSnapshot {
  room_id: string;
  revision: number;
  settings: RoomFallbackSettings;
  members: RoomFallbackMemberStatus[];
  /** Settings committed, but their attributed feed notice could not be written. */
  notice_warning?: string | null;
}

export interface SetRoomFallbacksRequest {
  room_id: string;
  revision: number;
  settings: RoomFallbackSettings;
}

export interface RoomFallbackOrigin {
  revision: number;
  session_id: string;
  run_id: string;
  generation: number;
  occurrence_id: string;
  scheduled_at: string;
}

/** An outside agent (Claude desktop, Codex, a script…) bound to one room. */
export interface RoomGuest {
  guest_id: string;
  label: string;
  room_id: string;
  created_at: string;
  last_seen: string | null;
}

/** `invite_room_guest`: the token is shown here once and never again. */
export interface RoomGuestInvitationView {
  guest: RoomGuest;
  guest_id: string;
  token: string;
  mcp_config_json: string;
  cli_example: string;
  /** A16: the one block to paste into any agent: join, take part, the MCP alternative. */
  agent_invitation: string;
}

/** Room save (A92): `export_room`. */
export interface RoomExportReport {
  path: string;
  label: string;
  members: number;
  records: number;
  bytes: number;
  /** How long room traffic waited while the feed was copied. */
  paused_ms: number;
  warnings: string[];
}

/** Room load (A92): `inspect_room_source`, one entry per room offered. */
export interface RoomSourceSummary {
  source_room_id: string;
  label: string;
  members: number;
  /** 0 and meaningless when `unavailable` is set. */
  records: number;
  /** Why this room of a runtime copy cannot be read; it cannot be chosen. */
  unavailable: string | null;
}

export type RoomImportMemberState = "resumes" | "history_needed" | "held" | "starts_fresh";

/** Room load (A92): `import_room`. A new room; nothing starts. */
export interface RoomImportReport {
  room: RoomSnapshot;
  members: {
    session_id: string;
    label: string;
    driver: DriverKind;
    permission_profile: PermissionProfile;
    state: RoomImportMemberState;
    note: string | null;
  }[];
  warnings: string[];
}

export type RoomMessageSender =
  | { kind: "operator" }
  | { kind: "session"; session_id: string }
  /** The label is the guest's name when it posted. */
  | { kind: "guest"; guest_id: string; label: string }
  /** A16: a local process of the operator's user through the operator entry. */
  | { kind: "local_process"; label: string }
  /** v3: PRIM-1 itself (automatic recovery notices); never anyone's instruction. */
  | { kind: "prim1" };

export type RoomDeliveryStatus =
  | "stored"
  | "pending"
  | "written"
  | "woken"
  | "fetched"
  | "held"
  | "failed";

export type RoomAckActor = { kind: "session"; session_id: string } | { kind: "operator" } | { kind: "local_process"; label: string }
  /** v3: PRIM-1 itself, disposing of an older continuation a newer one superseded. */
  | { kind: "prim1" };
export type RoomAckDisposition = "acked" | "declined";

export interface RoomAckSummary {
  actor: RoomAckActor;
  disposition: RoomAckDisposition;
  note: string;
  outcome_ref?: string | null;
  cursor: RoomFeedCursor;
}

export type RoomFeedItem =
  | { kind: "card"; request_id: string; card_id: string; author: string; card: RoomCardContent }
  | {
      kind: "message";
      message_id: string;
      request_id: string;
      sender: RoomMessageSender;
      content: string;
      recipient_ids: string[];
      membership_revision: number;
      fallback_origin?: RoomFallbackOrigin | null;
      /** Native recovery attribution; never inferred from editable content or request IDs. */
      continuation_origin?: { session_id: string; continuation_id: string } | null;
      /** v3 W3: the sender asked each recipient for an ACK; absent on legacy and system messages. */
      ack_requested?: boolean;
      /** v3 W5: guests this message is addressed to (stored ids). */
      guest_recipient_ids?: string[];
      /** v3 W5: their labels at admission, in the same order; kept after a revoke. */
      guest_recipient_labels?: string[];
    }
  | {
      /** v3 W3: a deferred wake and why; not a delivery receipt. */
      kind: "waiting";
      message_id: string;
      recipient_id: string;
      reason_code: string;
      reason: string;
      attempt?: { epoch: string; sequence: number } | null;
    }
  | {
      /** v3 W3b: the durable recipe of one pointer attempt, stored before any byte is typed. */
      kind: "submit_attempt";
      message_id: string;
      recipient_id: string;
      attempt: { epoch: string; sequence: number };
      typed: string;
      driver: string;
      conversation_id: string;
      working_dir: string;
      run_id?: string | null;
      covered?: string[];
    }
  | {
      /** v3 W3b: the member's own record shows this attempt's exact pointer as a prompt; not an ACK. */
      kind: "input_observed";
      message_id: string;
      recipient_id: string;
      attempt: { epoch: string; sequence: number };
      source: string;
      /** `<file name>@<byte offset>` of the matching record; absent outside the operator's own view. */
      locator?: string;
    }
  | {
      /** v3 W3: a recipient's ACK or decline, or the operator's disposition for it. */
      kind: "ack";
      request_id: string;
      message_id: string;
      recipient_id: string;
      actor: RoomAckActor;
      disposition: RoomAckDisposition;
      note: string;
      outcome_ref?: string | null;
    }
  | {
      /** v3 W3 (D6): the operator withdrew this message, with its reason. */
      kind: "cancel";
      request_id: string;
      message_id: string;
      actor: RoomAckActor;
      reason: string;
      /** Recipients it never reached. */
      not_typed?: string[];
      notified?: string[];
      /** Recipients it already reached; a withdrawal does not undo that. */
      reached?: string[];
    }
  | {
      /** v3 W5: a disposition of a message addressed to a guest, attributed to its actor. */
      kind: "guest_ack";
      request_id: string;
      message_id: string;
      guest_id: string;
      /** The guest's label as the message addressed it, kept after a revoke. */
      guest_label?: string;
      actor: { kind: "guest"; guest_id: string } | { kind: "operator" } | { kind: "local_process"; label: string };
      disposition: RoomAckDisposition;
      note: string;
      outcome_ref?: string | null;
    }
  | {
      kind: "membership";
      action: "joined" | "removed";
      session_id: string;
      membership_revision: number;
    }
  | {
      kind: "delivery";
      message_id: string;
      recipient_id: string;
      status: RoomDeliveryStatus;
      bytes_written: number;
      /** Explicit proof for a zero-byte Failed wake; absent on legacy receipts. */
      zero_write_confirmed?: boolean;
      error: string | null;
      run_id: string | null;
      generation: number | null;
      /** v3 W3: the member had already read every message this pointer
          counts: not typed on a Failed (never a failure, never retried);
          typed after those reads on a Written. */
      already_read?: boolean;
      /** v3 W2 #2: on a Written, typed past the cap while Codex's own title still read this busy state. */
      title_busy?: string;
      /** v3 W4 S2: on a Written, the Codex App Server's success response to this attempt's own
          turn/start named this turn. Nothing was typed; never model acceptance, Fetched or ACK. */
      app_server_turn?: string;
    };

export interface RoomFeedEvent {
  schema_version: number;
  room_id: string;
  cursor: RoomFeedCursor;
  item: RoomFeedItem;
  timestamp: string;
}

export interface RoomFeedPage {
  schema_version: number;
  room_id: string;
  cursor: RoomFeedCursor;
  gap: {
    reason: "evicted" | "epoch_reset";
    from_sequence: number | null;
    through_sequence: number | null;
  } | null;
  events: RoomFeedEvent[];
  cards?: RoomFeedEvent[];
  cards_cursor?: RoomFeedCursor | null;
  has_more: boolean;
  guests?: RoomGuest[];
}

export interface RoomCardContent {
  key: string;
  kind: "decision" | "progress" | "result" | "metric" | "alert" | "report" | "image" | "question";
  title: string;
  summary: string;
  body: string;
  state: "open" | "done";
  data: { done?: number; total?: number; value?: number; unit?: string; series?: number[]; png_base64?: string; alt?: string };
}

export interface ControlPlaneSnapshot {
  transport: string;
  endpoint: string;
}

export interface RuntimeSnapshot {
  sessions: SessionSnapshot[];
  rooms: RoomSnapshot[];
  workspace_preference: string;
  control_plane: ControlPlaneSnapshot | null;
  runtime_dir: string;
  audit_log_path: string;
  generated_at: string;
  /** Null is off; absent means this backend does not expose the setting. */
  idle_hibernate_minutes?: number | null;
}

/** One resource sample. Every process PRIM-1 started is counted in exactly
    one group. */
export interface TelemetrySnapshot {
  sampled_at: string;
  /** The logical processors every CPU percentage is a share of. */
  logical_cpus: number;
  core: TelemetryUsage;
  webview: TelemetryUsage;
  /** PRIM-1's other processes: the console host beside each terminal. */
  helpers: TelemetryUsage;
  /** Live runs in session order. */
  sessions: TelemetrySessionUsage[];
  total: TelemetryUsage;
  /** Listed process IDs with a failed read in at least one inventory.
      Memory/counts may be incomplete; run Job CPU includes unreadable members. */
  unreadable_processes: number;
  sampler_cost_ms: number;
}

export interface TelemetryUsage {
  /** Share of the whole machine since the previous sample, 0–100; null
      until a recent previous sample exists. */
  cpu_percent: number | null;
  /** Private bytes (commit charge) summed over the group's processes. */
  private_bytes: number;
  processes: number;
}

export interface TelemetrySessionUsage {
  session_id: string;
  label: string;
  driver: DriverKind;
  cpu_percent: number | null;
  private_bytes: number;
  processes: number;
  /** Highest sampled CPU since the run started (sampled only on screen). */
  peak_cpu_percent: number | null;
  /** The Job's peak commit since the run started, or a higher sample. */
  peak_private_bytes: number;
}

export interface HeartbeatSessionSummary {
  name: string;
  lifecycle_state: LifecycleState;
  work_state: WorkState | null;
  process_id: number | null;
  last_activity_at: string | null;
}

/** A conversation reference the next Launch cannot resume, and why. */
export interface SessionRecovery {
  original_id: string | null;
  provenance: string;
  reason: string;
}

/** What the operator asked a launch to use: requested, never observed. */
export interface SessionLaunchChoices {
  model: string | null;
  effort: string | null;
  brief: SessionBriefChoice;
}

export type SessionBriefChoice =
  | { mode: "room_default" }
  | { mode: "custom"; text: string }
  | { mode: "none" };

export interface StartSessionRequest {
  session_id: string;
  /** Start a NEW harness conversation instead of resuming the stored one. */
  fresh?: boolean;
  /** After PATH + cmd-shim miss, walk the usual install locations. */
  search?: boolean;
  /** Omitted keeps the saved choices; a stopped session saves new ones first. */
  launch_choices?: SessionLaunchChoices | null;
}

export interface StopSessionRequest {
  session_id: string;
}

export interface RestartSessionRequest {
  session_id: string;
}

export interface CreateSessionRequest {
  label?: string | null;
  driver: DriverKind;
  permission_profile: PermissionProfile;
  linux_working_directory?: string | null;
  /** Born already a member of this room ("+" inside a room). */
  room_id?: string | null;
  /** Omitted: room default brief for agents, none for Terminal. */
  launch_choices?: SessionLaunchChoices | null;
  /** A16: grants saved with the new session; omitted gives none. */
  permissions?: MemberPermissions | null;
  /** v3 W4: Codex only; omitted means "pty". */
  codex_transport?: CodexTransport;
}

export interface RenameSessionRequest {
  session_id: string;
  label: string;
}

export interface ChooseSessionWorkingDirectoryRequest {
  session_id: string;
}

export interface SetSessionLinuxWorkingDirectoryRequest {
  session_id: string;
  linux_working_directory: string;
}

export interface SetSessionPermissionRequest {
  session_id: string;
  permission_profile: PermissionProfile;
}

/** Operator-only `set_member_permissions`. */
export interface SetMemberPermissionsRequest {
  session_id: string;
  roam: boolean;
  maintain: boolean;
  manage: boolean;
}

export interface MoveSessionRequest {
  session_id: string;
  new_index: number;
}

export interface DeleteSessionRequest {
  session_id: string;
}

export interface CreateRoomRequest {
  label?: string | null;
  member_ids: string[];
  /** Enable the room default brief for a fresh conversation launch. */
  brief_on_join: boolean;
  /** Edited brief for this room; null means the canonical brief. */
  brief_template?: string | null;
}

export interface RenameRoomRequest {
  room_id: string;
  label: string;
}

export interface SetRoomResumeRequest {
  room_id: string;
  resume_on_restart: boolean;
}

/** A room's launch-brief settings (S5). brief_template null = the default. */
export interface RoomBriefSettings {
  room_id: string;
  brief_on_join: boolean;
  brief_template: string | null;
}

export interface ReadRoomBriefRequest {
  room_id: string;
}

export interface UpdateRoomBriefRequest {
  room_id: string;
  brief_on_join: boolean;
  brief_template: string | null;
}

export interface MoveRoomRequest {
  room_id: string;
  new_index: number;
}

export interface DeleteRoomRequest {
  room_id: string;
}

export interface AddRoomMemberRequest {
  room_id: string;
  session_id: string;
}

export interface RemoveRoomMemberRequest {
  room_id: string;
  session_id: string;
}

export interface InviteRoomGuestRequest {
  room_id: string;
  label: string | null;
}

export interface RevokeRoomGuestRequest {
  guest_id: string;
}

export interface ReadRoomFeedRequest {
  room_id: string;
  cursor?: RoomFeedCursor | null;
}

export interface PostRoomMessageRequest {
  room_id: string;
  content: string;
  request_id?: string | null;
}

export type RoomRecipientSelection =
  | { kind: "one"; session_id: string }
  | { kind: "all" };

export interface DeliverRoomMessageRequest {
  room_id: string;
  recipients: RoomRecipientSelection;
  content: string;
  request_id?: string | null;
}

export interface RetryRoomDeliveryRequest {
  room_id: string;
  message_id: string;
  recipient_id: string;
  expected_receipt: RoomFeedCursor;
}

export interface RoomPostResult {
  room_id: string;
  message_id: string;
  request_id: string;
  replayed: boolean;
  cursor: RoomFeedCursor;
}

export interface BriefRoomMemberRequest {
  room_id: string;
  session_id: string;
}

export interface RoomDeliveryFailure {
  recipient_id: string;
  bytes_written: number;
  error: string;
}

export interface RoomDeliveryReceipt {
  recipient_id: string;
  status: RoomDeliveryStatus;
  bytes_written: number;
  error: string | null;
  run_id: string | null;
  generation: number | null;
  recorded: boolean;
  /** Why a Stored wake is queued at delivery; only in the delivery's own result. */
  queued_because?: string | null;
  /** v3 W3: the latest deferral reason code, until a transport outcome supersedes it. */
  waiting?: string | null;
  /** v3 W3: why this recipient's wake was last held. */
  hold?: string | null;
  /** v3 W3: as on the delivery item; an already-read Failed is no failure. */
  already_read?: boolean;
  /** v3 W2 #2: as on the delivery item. */
  title_busy?: string;
  /** v3 W4 S2: as on the delivery item. */
  app_server_turn?: string;
}

export interface RoomDeliveryResult {
  room_id: string;
  message_id: string;
  request_id: string;
  replayed: boolean;
  cursor: RoomFeedCursor;
  recipient_count: number;
  written_count: number;
  failures: RoomDeliveryFailure[];
  receipts: RoomDeliveryReceipt[];
}

export interface SendInputRequest {
  session_id: string;
  input: string;
}

export interface OutputCredit {
  epoch: string;
  run: { session_id: string; run_id: string; generation: number };
  tickets: string[];
}

export type RuntimeEvent =
  | {
      event: "session_output_draining";
      identity: RunEventIdentity;
      session: string;
      timestamp: string;
    }
  | {
      event: "session_output";
      output_credit?: OutputCredit;
      identity: RunEventIdentity;
      session: string;
      chunk: string;
      synthetic: boolean;
      timestamp: string;
    }
  | {
      event: "ui_output_gap";
      identity: RunEventIdentity;
      session: string;
      dropped_events: number;
      timestamp: string;
    }
  | {
      event: "session_state";
      identity: RunEventIdentity;
      session: string;
      state: LifecycleState;
      reason: string;
      timestamp: string;
    }
  | {
      event: "session_exit";
      identity: RunEventIdentity;
      session: string;
      process_id: number | null;
      exit_code: number | null;
      signal: number | null;
      success: boolean;
      reason: SessionExitReason;
      requested: boolean;
      timestamp: string;
    }
  | {
      /** v3: what a member's snapshot presents changed (W2 #4 notice expiry, J4 provider error); refresh it. */
      event: "session_presentation_changed";
      session_id: string;
      run_id: string;
      session: string;
      timestamp: string;
    }
  | {
      event: "session_work_state";
      identity: RunEventIdentity;
      session: string;
      state: WorkState;
      detail: string | null;
      previous_state: WorkState | null;
      timestamp: string;
    }
  | {
      event: "supervisor_heartbeat";
      wrapper_pid: number;
      uptime_secs: number;
      sessions: HeartbeatSessionSummary[];
      timestamp: string;
    }
  | {
      event: "supervisor_alert";
      alert_type: SupervisorAlertType;
      request_id: string | null;
      session: string | null;
      action: string | null;
      last_work_state: WorkState | null;
      last_session_state: LifecycleState | null;
      message: string;
      severity: AlertSeverity;
      timestamp: string;
    }
  | {
      event: "session_created";
      schema_version: number;
      session: SessionSnapshot;
      timestamp: string;
    }
  | {
      event: "session_renamed";
      schema_version: number;
      session_id: string;
      old_label: string;
      new_label: string;
      timestamp: string;
    }
  | {
      event: "session_moved";
      schema_version: number;
      session_id: string;
      old_index: number;
      new_index: number;
      timestamp: string;
    }
  | {
      event: "session_permission_changed";
      schema_version: number;
      session_id: string;
      old_profile: PermissionProfile;
      new_profile: PermissionProfile;
      timestamp: string;
    }
  | {
      event: "session_member_permissions_changed";
      schema_version: number;
      session_id: string;
      old_permissions: MemberPermissions;
      new_permissions: MemberPermissions;
      timestamp: string;
    }
  | {
      event: "session_working_directory_changed";
      schema_version: number;
      session_id: string;
      old_working_dir: string;
      new_working_dir: string;
      timestamp: string;
    }
  | {
      event: "session_deleted";
      schema_version: number;
      session_id: string;
      label: string;
      timestamp: string;
    }
  | {
      event: "room_created";
      schema_version: number;
      room: RoomSnapshot;
      timestamp: string;
    }
  | {
      event: "room_renamed";
      schema_version: number;
      room_id: string;
      old_label: string;
      new_label: string;
      timestamp: string;
    }
  | {
      event: "room_moved";
      schema_version: number;
      room_id: string;
      old_index: number;
      new_index: number;
      timestamp: string;
    }
  | {
      event: "room_member_added";
      schema_version: number;
      room_id: string;
      session_id: string;
      membership_revision: number;
      timestamp: string;
    }
  | {
      event: "room_member_removed";
      schema_version: number;
      room_id: string;
      session_id: string;
      membership_revision: number;
      timestamp: string;
    }
  | {
      event: "room_deleted";
      schema_version: number;
      room_id: string;
      label: string;
      timestamp: string;
    }
  | {
      event: "room_history_changed";
      schema_version: number;
      room_id: string;
      timestamp: string;
    }
  | {
      event: "room_guests_changed";
      schema_version: number;
      room_id: string;
      guests: RoomGuest[];
      timestamp: string;
    }
  | {
      event: "room_fallbacks_changed";
      room_id: string;
    }
  | {
      event: "room_feed_event";
      feed_event: RoomFeedEvent;
    }
  | {
      event: "routed_message";
      id: string;
      from: string;
      to: string;
      scope: MessageScope;
      content: string;
      timestamp: string;
    }
  | {
      event: "route_delivery";
      request_id: string;
      route_id: string;
      from: string;
      logical_to: string;
      scope: MessageScope;
      recipient: string | null;
      recipient_index: number;
      recipient_count: number;
      payload_part_count: number;
      phase: RouteDeliveryPhase;
      bytes_written: number;
      error: string | null;
      timestamp: string;
    }
  | {
      event: "dispatch_attempt";
      request_id: string;
      action: string;
      from: string;
      target_session: string;
      target_lifecycle_state_before: LifecycleState;
      target_work_state_before: WorkState | null;
      target_last_activity_at: string | null;
      last_route_from_target_at: string | null;
      overlap: boolean;
      reason: string | null;
      timestamp: string;
    }
  | {
      event: "system_log";
      level: LogLevel;
      message: string;
      timestamp: string;
    }
  | {
      event: "control_plane_ready";
      endpoint: string;
      transport: string;
      timestamp: string;
    }
  | {
      event: "upgrade_status_changed";
      status: UpgradeStatus | null;
      timestamp: string;
    };

/** A108 self-upgrade stages, as the supervisor reports them. */
export type UpgradeStage = "notice" | "closing" | "held" | "resuming" | "resumed" | "cancelled" | "failed";

export interface UpgradeStatus {
  attempt_id: string;
  stage: UpgradeStage;
  candidate_sha256: string;
  requester_label: string;
  /** RFC 3339: PRIM-1 does not close before this. */
  notice_until: string;
  expires_at: string;
  message: string;
  can_cancel: boolean;
  can_abandon: boolean;
  /** Members the upgrade resumes; the restart resume leaves them alone. */
  member_session_ids: string[];
}
