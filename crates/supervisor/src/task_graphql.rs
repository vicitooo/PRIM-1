//! GraphQL over the task store. Agents use the agent root (queries and
//! mutations) through the pane sideband `tasks` tool; the desktop uses the
//! read-only root, whose schema has no mutation type at all.
//!
//! Work is bounded before and during execution, because juniper itself
//! limits neither nesting, fragment expansion nor response size:
//! - request bytes (query, variables, operation name);
//! - a lexical pass over the raw query, before juniper's recursive parser
//!   and validator see it: nesting depth, fragment spreads, fragment
//!   definitions, no fragment cycles, and the size every definition (used or
//!   not) reaches once its fragments are expanded;
//! - an object field (one with its own selection) appears once per response
//!   name in each selection set, so the output meter reads its real keys;
//! - introspection is disabled (its types are recursive); the schema is
//!   summarized in the tool description instead;
//! - a resolver budget: every `task`, `tasks`, `createTask` and `updateTask`
//!   field draws one unit when it starts, whether it then succeeds or fails;
//! - an output meter: every object charges its response keys (aliases
//!   included) before it is built, a list charges them once per element
//!   before any element exists, and fields charge a lower bound of the value
//!   bytes they add. Once the cap is passed everything resolves empty, so
//!   nothing large is built; the request then fails with the cap error, still
//!   naming each confirmed current task result (including replays) so the
//!   caller knows what took effect. The serialized response is checked
//!   against the cap again before it is returned.
//!
//! The schema is not recursive: `parent` is an id string and no field inside
//! `Task`, `LogEntry`, `TaskPage` or `UnreadableFile` returns another of them.
//! Timestamps are RFC 3339 strings, as stored.
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
};

use juniper::{
    DefaultScalarValue, Document, EmptyMutation, EmptySubscription, Executor, FieldError, FieldResult, GraphQLEnum,
    GraphQLInputObject, GraphQLType, Object, OperationType, RootNode, Selection, Value, Variables,
    graphql_object, http::GraphQLResponse,
};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value as Json, json};

use crate::tasks::{
    self, LIST_MAX_LIMIT, NewTask, TaskError, TaskFilter, TaskPatch, TaskState, TaskStore,
};

pub const QUERY_MAX_BYTES: usize = 16 * 1024;
pub const VARIABLES_MAX_BYTES: usize = 128 * 1024;
pub const OPERATION_NAME_MAX_CHARS: usize = 128;
pub const OUTPUT_MAX_BYTES: usize = 1024 * 1024;
pub const RESOLVER_BUDGET: u32 = 20;
const NESTING_MAX: usize = 12;
const SPREADS_MAX: usize = 32;
const FRAGMENTS_MAX: usize = 16;
// Names (fields, aliases, arguments, ...) after fragment expansion. A query of
// at most 16 KiB holds at most 8192 names, so only fragments can exceed this.
const EXPANDED_NAMES_MAX: usize = 8192;
const DEFAULT_LIMIT: i32 = 50;

pub const NO_TASK_FOLDER: &str =
    "No task folder is set. Choose one in PRIM-1 Settings → Task folder.";

/// Called before each mutation, with no lock held; an error stops the change.
/// The sideband uses it to confirm the calling pane is still the current run.
pub type MutationGuard = Box<dyn Fn() -> Result<(), String> + Send + Sync>;

/// A confirmed current task result: a mutation in this request that the
/// store confirmed, with the task's id and revision as the store returned it.
/// That is a change made by this request, or the replay of an earlier change
/// with the same mutationId, which returns the task as it is now (its revision
/// can be later than the one that change produced). The store does not say
/// which, so neither does this. Reported even when the response itself cannot
/// be returned, so the caller knows what took effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Confirmed {
    pub id: String,
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation_id: Option<String>,
}

#[derive(Debug)]
pub struct TaskOutcome {
    /// The standard GraphQL response object: `data` and/or `errors`.
    pub result: Json,
    /// Every confirmed current task result (including replays), in order.
    pub committed: Vec<Confirmed>,
}

impl TaskOutcome {
    pub fn errors(&self) -> &[Json] {
        self.result
            .get("errors")
            .and_then(Json::as_array)
            .map_or(&[], Vec::as_slice)
    }

    pub fn first_error_message(&self) -> Option<&str> {
        self.errors().first()?.get("message")?.as_str()
    }

    fn rejected(message: &str, code: &str) -> Self {
        Self { result: error_result(message, code), committed: Vec::new() }
    }
}

/// The result for a request that arrives while no task folder is open.
pub fn no_task_folder(message: &str) -> TaskOutcome {
    TaskOutcome::rejected(message, "NO_TASK_FOLDER")
}

/// Queries and mutations, attributed to `actor`. `before_mutation` runs
/// immediately before every mutation reaches the store.
pub fn execute_agent(
    store: Arc<TaskStore>,
    actor: &str,
    before_mutation: Option<MutationGuard>,
    query: &str,
    variables: Option<&Json>,
    operation_name: Option<&str>,
) -> TaskOutcome {
    let context = TaskContext::new(store, Some(actor.to_owned()), before_mutation, OUTPUT_MAX_BYTES);
    run(&AGENT_ROOT, context, query, variables, operation_name)
}

/// Queries only. The schema has no mutation type, so any mutation operation
/// fails validation before anything executes.
pub fn execute_read_only(
    store: Arc<TaskStore>,
    query: &str,
    variables: Option<&Json>,
    operation_name: Option<&str>,
) -> TaskOutcome {
    let context = TaskContext::new(store, None, None, OUTPUT_MAX_BYTES);
    run(&READ_ONLY_ROOT, context, query, variables, operation_name)
}

/// Request size bounds; the sideband also checks them while authorizing.
pub fn check_request(query: &str, variables: Option<&Json>, operation_name: Option<&str>) -> Result<(), String> {
    if query.trim().is_empty() {
        return Err("query must not be empty".into());
    }
    if query.len() > QUERY_MAX_BYTES {
        return Err(format!("query is {} bytes; the limit is 16 KiB ({QUERY_MAX_BYTES} bytes)", query.len()));
    }
    match variables {
        None | Some(Json::Null) => {}
        Some(value @ Json::Object(_)) => {
            let bytes = serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len());
            if bytes > VARIABLES_MAX_BYTES {
                return Err(format!(
                    "variables are {bytes} bytes as JSON; the limit is 128 KiB ({VARIABLES_MAX_BYTES} bytes)"
                ));
            }
        }
        Some(_) => return Err("variables must be a JSON object".into()),
    }
    if let Some(name) = operation_name {
        let chars = name.chars().count();
        if chars > OPERATION_NAME_MAX_CHARS {
            return Err(format!("operation_name is {chars} characters; the limit is {OPERATION_NAME_MAX_CHARS}"));
        }
    }
    Ok(())
}

type AgentRoot = RootNode<Query, Mutation, EmptySubscription<TaskContext>>;
type ReadOnlyRoot = RootNode<Query, EmptyMutation<TaskContext>, EmptySubscription<TaskContext>>;

static AGENT_ROOT: LazyLock<AgentRoot> =
    LazyLock::new(|| RootNode::new(Query, Mutation, EmptySubscription::new()).disable_introspection());
static READ_ONLY_ROOT: LazyLock<ReadOnlyRoot> = LazyLock::new(|| {
    RootNode::new(Query, EmptyMutation::new(), EmptySubscription::new()).disable_introspection()
});

fn run<M>(
    root: &RootNode<Query, M, EmptySubscription<TaskContext>>,
    context: TaskContext,
    query: &str,
    variables: Option<&Json>,
    operation_name: Option<&str>,
) -> TaskOutcome
where
    M: GraphQLType<Context = TaskContext, TypeInfo = ()>,
{
    if let Err(message) = check_request(query, variables, operation_name) {
        return TaskOutcome::rejected(&message, "BAD_REQUEST");
    }
    if let Err(message) = check_document_shape(query) {
        return TaskOutcome::rejected(&message, "QUERY_TOO_COMPLEX");
    }
    // A document that does not parse is left to juniper, which reports why.
    if let Ok(document) = juniper::parser::parse_document_source::<DefaultScalarValue>(query, &root.schema) {
        if let Err(message) = check_repeated_object_fields(&document) {
            return TaskOutcome::rejected(&message, "QUERY_TOO_COMPLEX");
        }
        if root.schema.concrete_mutation_type().is_none() && selects_mutation(&document, operation_name) {
            return TaskOutcome::rejected("this task view is read-only: its schema has no mutations", "READ_ONLY");
        }
    }
    let variables: Variables = match variables {
        None | Some(Json::Null) => Variables::new(),
        Some(value) => match serde_json::from_value(value.clone()) {
            Ok(variables) => variables,
            Err(error) => {
                return TaskOutcome::rejected(&format!("variables are not valid GraphQL input values: {error}"), "BAD_VARIABLES");
            }
        },
    };
    let response = GraphQLResponse::from_result(juniper::execute_sync(
        query,
        operation_name,
        root,
        &variables,
        &context,
    ));
    let confirmed = std::mem::take(&mut *context.confirmed.lock());
    let not_run = context.not_run.load(Ordering::Relaxed);
    let cap = context.output_cap;
    // Over the meter, the response holds placeholders and is never returned.
    // Otherwise the serialized size is the backstop for anything the meter's
    // lower bound did not count.
    let result = if context.over_cap.load(Ordering::Relaxed) {
        None
    } else {
        serde_json::to_value(&response)
            .ok()
            .filter(|value| serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= cap))
    };
    let result = result.unwrap_or_else(|| over_cap_result(cap, &confirmed, not_run));
    TaskOutcome { result, committed: confirmed }
}

struct TaskContext {
    store: Arc<TaskStore>,
    /// None on the read-only root.
    actor: Option<String>,
    before_mutation: Option<MutationGuard>,
    resolutions: AtomicU32,
    output_cap: usize,
    charged: AtomicUsize,
    over_cap: AtomicBool,
    not_run: AtomicBool,
    confirmed: Mutex<Vec<Confirmed>>,
}

impl juniper::Context for TaskContext {}

impl TaskContext {
    fn new(
        store: Arc<TaskStore>,
        actor: Option<String>,
        before_mutation: Option<MutationGuard>,
        output_cap: usize,
    ) -> Self {
        Self {
            store,
            actor,
            before_mutation,
            resolutions: AtomicU32::new(0),
            output_cap,
            charged: AtomicUsize::new(0),
            over_cap: AtomicBool::new(false),
            not_run: AtomicBool::new(false),
            confirmed: Mutex::new(Vec::new()),
        }
    }

    /// `Ok(false)`: the response is already over the cap, so this field
    /// resolves empty and a mutation does not run. Otherwise draws one unit.
    /// Each root field calls this exactly once, first, before anything that
    /// can fail, so a failed field spends its unit like a successful one.
    fn admit(&self, mutation: bool) -> FieldResult<bool> {
        if self.over_cap.load(Ordering::Relaxed) {
            if mutation {
                self.not_run.store(true, Ordering::Relaxed);
            }
            return Ok(false);
        }
        if self.resolutions.fetch_add(1, Ordering::Relaxed) >= RESOLVER_BUDGET {
            return Err(field_error(
                format!(
                    "a task request may resolve at most {RESOLVER_BUDGET} task, tasks, createTask or updateTask fields; split it into several requests"
                ),
                "BUDGET_EXCEEDED",
                [],
            ));
        }
        Ok(true)
    }

    /// Adds a lower bound of the output bytes; false once past the cap.
    fn charge(&self, bytes: usize) -> bool {
        if self.over_cap.load(Ordering::Relaxed) {
            return false;
        }
        // execute_sync resolves one field at a time; no concurrent charges.
        let total = self.charged.load(Ordering::Relaxed).saturating_add(bytes);
        self.charged.store(total, Ordering::Relaxed);
        if total > self.output_cap {
            self.over_cap.store(true, Ordering::Relaxed);
            return false;
        }
        true
    }

    fn text<'a>(&self, value: &'a str) -> &'a str {
        if self.charge(value.len() + 2) { value } else { "" }
    }

    fn optional<'a>(&self, value: &'a Option<String>) -> Option<&'a str> {
        let bytes = value.as_ref().map_or(4, |value| value.len() + 2);
        if self.charge(bytes) { value.as_deref() } else { None }
    }

}

/// The bytes the response keys of one object add: `{}`, and per distinct
/// response key (alias or field name) its quoted text, a colon and a comma.
/// A key repeated in the selection is merged into one in the response.
/// Juniper's look-ahead reads the first occurrence of this field's response
/// name in its selection set; `check_repeated_object_fields` makes that the
/// only occurrence.
fn object_key_bytes(executor: &Executor<'_, '_, TaskContext>) -> usize {
    let children = executor.look_ahead().children();
    let mut keys = HashSet::new();
    let bytes = children.names().filter(|name| keys.insert(*name)).map(|name| name.len() + 4).sum::<usize>();
    bytes.saturating_add(1).max(2)
}

/// A root field's object: its own key in `data` plus the object's keys,
/// charged before the object is built. False once past the cap.
fn charge_root_object(executor: &Executor<'_, '_, TaskContext>) -> bool {
    let key = executor.look_ahead().field_name().len() + 4;
    executor.context().charge(key.saturating_add(object_key_bytes(executor)))
}

/// A list of objects: every element's keys, `[]` and separators, charged
/// before any element exists. Empty once past the cap.
fn objects<'a, T>(executor: &Executor<'_, '_, TaskContext>, items: &'a [T]) -> Vec<&'a T> {
    let element = object_key_bytes(executor).saturating_add(1);
    if executor.context().charge(items.len().saturating_mul(element).saturating_add(2)) {
        items.iter().collect()
    } else {
        Vec::new()
    }
}

/// Task lifecycle state. `dropped` replaces deletion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, GraphQLEnum)]
#[graphql(name = "TaskState")]
enum GqlTaskState {
    #[graphql(name = "open")]
    Open,
    #[graphql(name = "doing")]
    Doing,
    #[graphql(name = "blocked")]
    Blocked,
    #[graphql(name = "review")]
    Review,
    #[graphql(name = "done")]
    Done,
    #[graphql(name = "dropped")]
    Dropped,
}

impl From<TaskState> for GqlTaskState {
    fn from(state: TaskState) -> Self {
        match state {
            TaskState::Open => Self::Open,
            TaskState::Doing => Self::Doing,
            TaskState::Blocked => Self::Blocked,
            TaskState::Review => Self::Review,
            TaskState::Done => Self::Done,
            TaskState::Dropped => Self::Dropped,
        }
    }
}

impl From<GqlTaskState> for TaskState {
    fn from(state: GqlTaskState) -> Self {
        match state {
            GqlTaskState::Open => Self::Open,
            GqlTaskState::Doing => Self::Doing,
            GqlTaskState::Blocked => Self::Blocked,
            GqlTaskState::Review => Self::Review,
            GqlTaskState::Done => Self::Done,
            GqlTaskState::Dropped => Self::Dropped,
        }
    }
}

struct TaskObject {
    task: tasks::Task,
    log: Vec<LogEntryObject>,
}

impl From<tasks::Task> for TaskObject {
    fn from(mut task: tasks::Task) -> Self {
        let log = std::mem::take(&mut task.log).into_iter().map(LogEntryObject).collect();
        Self { task, log }
    }
}

/// One task record.
#[graphql_object(name = "Task", context = TaskContext, scalar = DefaultScalarValue)]
impl TaskObject {
    /// Task id, such as T-7.
    fn id(&self, context: &TaskContext) -> &str {
        context.text(&self.task.id)
    }

    fn title(&self, context: &TaskContext) -> &str {
        context.text(&self.task.title)
    }

    /// The original words, verbatim. Fixed at create; no operation changes them.
    fn request(&self, context: &TaskContext) -> &str {
        context.text(&self.task.request)
    }

    /// Where the request came from (room and time, or a file path). Fixed at create.
    fn source(&self, context: &TaskContext) -> &str {
        context.text(&self.task.source)
    }

    /// What done means.
    fn outcome(&self, context: &TaskContext) -> &str {
        context.text(&self.task.outcome)
    }

    fn state(&self, context: &TaskContext) -> GqlTaskState {
        context.charge(6);
        self.task.state.into()
    }

    fn owner(&self, context: &TaskContext) -> Option<&str> {
        context.optional(&self.task.owner)
    }

    fn reviewer(&self, context: &TaskContext) -> Option<&str> {
        context.optional(&self.task.reviewer)
    }

    /// Parent task id, or null for a top-level task.
    fn parent(&self, context: &TaskContext) -> Option<&str> {
        context.optional(&self.task.parent)
    }

    /// Evidence links or references; a set, in the order added.
    fn evidence(&self, context: &TaskContext) -> Vec<&str> {
        let bytes = self.task.evidence.iter().map(|link| link.len() + 3).sum::<usize>() + 2;
        if context.charge(bytes) {
            self.task.evidence.iter().map(String::as_str).collect()
        } else {
            Vec::new()
        }
    }

    fn created_at(&self, context: &TaskContext) -> &str {
        context.text(&self.task.created_at)
    }

    fn updated_at(&self, context: &TaskContext) -> &str {
        context.text(&self.task.updated_at)
    }

    /// Send this as expectedRevision to update the task.
    fn revision(&self, context: &TaskContext) -> FieldResult<i32> {
        context.charge(1);
        graphql_int("revision", self.task.revision)
    }

    /// Change history, oldest first.
    fn log(&self, executor: &Executor<'_, '_, TaskContext>) -> Vec<&LogEntryObject> {
        objects(executor, &self.log)
    }
}

struct LogEntryObject(tasks::LogEntry);

/// One change to a task.
#[graphql_object(name = "LogEntry", context = TaskContext, scalar = DefaultScalarValue)]
impl LogEntryObject {
    fn at(&self, context: &TaskContext) -> &str {
        context.text(&self.0.at)
    }

    /// Who made the change: the calling pane's identity, never free text.
    fn by(&self, context: &TaskContext) -> &str {
        context.text(&self.0.by)
    }

    /// Change summary written by PRIM-1.
    fn what(&self, context: &TaskContext) -> &str {
        context.text(&self.0.what)
    }

    /// The caller's own words for this change.
    fn note(&self, context: &TaskContext) -> Option<&str> {
        context.optional(&self.0.note)
    }

    fn mutation_id(&self, context: &TaskContext) -> Option<&str> {
        context.optional(&self.0.mutation_id)
    }
}

struct PageObject {
    items: Vec<TaskObject>,
    total: usize,
    unreadable: Vec<UnreadableObject>,
}

/// One page of tasks.
#[graphql_object(name = "TaskPage", context = TaskContext, scalar = DefaultScalarValue)]
impl PageObject {
    /// Matching tasks on this page, ordered by numeric id.
    fn items(&self, executor: &Executor<'_, '_, TaskContext>) -> Vec<&TaskObject> {
        objects(executor, &self.items)
    }

    /// Readable tasks matching the filter, before paging.
    fn total(&self, context: &TaskContext) -> FieldResult<i32> {
        context.charge(1);
        graphql_int("total", self.total as u64)
    }

    /// Every task file in the folder that cannot be used, whatever the filter.
    fn unreadable(&self, executor: &Executor<'_, '_, TaskContext>) -> Vec<&UnreadableObject> {
        objects(executor, &self.unreadable)
    }
}

struct UnreadableObject(tasks::UnreadableFile);

/// A task file that does not validate. It is reported and never rewritten.
#[graphql_object(name = "UnreadableFile", context = TaskContext, scalar = DefaultScalarValue)]
impl UnreadableObject {
    fn file(&self, context: &TaskContext) -> &str {
        context.text(&self.0.file)
    }

    fn error(&self, context: &TaskContext) -> &str {
        context.text(&self.0.error)
    }
}

struct Query;

#[graphql_object(context = TaskContext, scalar = DefaultScalarValue)]
impl Query {
    /// One task by id, such as "T-7". A missing task is an error.
    fn task(executor: &Executor<'_, '_, TaskContext>, id: String) -> FieldResult<Option<TaskObject>> {
        let context = executor.context();
        if !context.admit(false)? {
            return Ok(None);
        }
        let task = context.store.get(&id).map_err(task_error)?;
        Ok(charge_root_object(executor).then(|| task.into()))
    }

    /// Tasks ordered by numeric id, filtered by every argument given. `limit`
    /// is 0 to 200; a larger value is an error, not a silent cut.
    fn tasks(
        executor: &Executor<'_, '_, TaskContext>,
        state: Option<GqlTaskState>,
        owner: Option<String>,
        parent: Option<String>,
        #[graphql(default = 0)] offset: i32,
        #[graphql(default = DEFAULT_LIMIT)] limit: i32,
    ) -> FieldResult<Option<PageObject>> {
        let context = executor.context();
        if !context.admit(false)? {
            return Ok(None);
        }
        let offset = usize::try_from(offset)
            .map_err(|_| field_error("offset must be 0 or more", "INVALID", []))?;
        let limit = usize::try_from(limit)
            .ok()
            .filter(|limit| *limit <= LIST_MAX_LIMIT)
            .ok_or_else(|| {
                field_error(
                    format!("limit must be 0 to {LIST_MAX_LIMIT}; read further pages with offset"),
                    "INVALID",
                    [],
                )
            })?;
        let filter = TaskFilter { state: state.map(Into::into), owner, parent };
        let page = context.store.list(&filter, offset, limit).map_err(task_error)?;
        Ok(charge_root_object(executor).then(|| PageObject {
            items: page.items.into_iter().map(Into::into).collect(),
            total: page.total,
            unreadable: page.unreadable.into_iter().map(UnreadableObject).collect(),
        }))
    }
}

struct Mutation;

#[graphql_object(context = TaskContext, scalar = DefaultScalarValue)]
impl Mutation {
    /// Create a task in state open at revision 1. `request` is stored
    /// verbatim and can never change. Reusing a mutationId with the same
    /// input creates nothing and returns that task as it is now.
    fn create_task(
        executor: &Executor<'_, '_, TaskContext>,
        input: NewTaskInput,
        mutation_id: Option<String>,
    ) -> FieldResult<Option<TaskObject>> {
        if !executor.context().admit(true)? {
            return Ok(None);
        }
        mutate(executor, mutation_id, |store, actor, mutation_id| {
            store.create(actor, input.into(), mutation_id)
        })
    }

    /// Change a task you have read. `expectedRevision` is the revision you
    /// read; if the task changed since, nothing is written and the error
    /// carries `currentRevision`. Reusing a mutationId with the same change
    /// changes nothing again and returns the task as it is now.
    fn update_task(
        executor: &Executor<'_, '_, TaskContext>,
        id: String,
        expected_revision: i32,
        patch: TaskPatchInput,
        mutation_id: Option<String>,
    ) -> FieldResult<Option<TaskObject>> {
        if !executor.context().admit(true)? {
            return Ok(None);
        }
        let expected_revision = u64::try_from(expected_revision).map_err(|_| {
            field_error("expectedRevision must be the revision you read (1 or more)", "INVALID", [])
        })?;
        let patch = patch.into_patch()?;
        mutate(executor, mutation_id, |store, actor, mutation_id| {
            store.update(actor, &id, expected_revision, patch, mutation_id)
        })
    }
}

/// Runs one admitted mutation: the caller has already drawn its unit.
fn mutate(
    executor: &Executor<'_, '_, TaskContext>,
    mutation_id: Option<String>,
    change: impl FnOnce(&TaskStore, &str, Option<&str>) -> Result<tasks::Task, TaskError>,
) -> FieldResult<Option<TaskObject>> {
    let context = executor.context();
    let actor = context
        .actor
        .as_deref()
        .ok_or_else(|| field_error("this task view is read-only", "READ_ONLY", []))?;
    if let Some(guard) = &context.before_mutation {
        guard().map_err(|message| field_error(format!("{message}; nothing was changed"), "CALLER_STALE", []))?;
    }
    let task = change(&context.store, actor, mutation_id.as_deref()).map_err(task_error)?;
    context.confirmed.lock().push(Confirmed {
        id: task.id.clone(),
        revision: task.revision,
        mutation_id,
    });
    Ok(charge_root_object(executor).then(|| task.into()))
}

/// A new task. Owner, reviewer and parent are optional; parent must be an
/// existing task id.
#[derive(GraphQLInputObject)]
struct NewTaskInput {
    title: String,
    /// The original words, verbatim (up to 64 KiB). Never changes afterwards.
    request: String,
    /// Where the request came from, such as "room 444, 2026-10-14T19:48Z".
    source: String,
    /// What done means.
    outcome: Option<String>,
    owner: Option<String>,
    reviewer: Option<String>,
    parent: Option<String>,
    evidence: Option<Vec<String>>,
}

impl From<NewTaskInput> for NewTask {
    fn from(input: NewTaskInput) -> Self {
        NewTask {
            title: input.title,
            request: input.request,
            source: input.source,
            outcome: input.outcome.unwrap_or_default(),
            owner: input.owner,
            reviewer: input.reviewer,
            parent: input.parent,
            evidence: input.evidence.unwrap_or_default(),
        }
    }
}

/// A change to a task. Omitted or null fields stay unchanged. To clear owner,
/// reviewer or parent, set clearOwner, clearReviewer or clearParent to true.
/// There is no request or source field: those never change.
#[derive(GraphQLInputObject)]
struct TaskPatchInput {
    title: Option<String>,
    state: Option<GqlTaskState>,
    owner: Option<String>,
    clear_owner: Option<bool>,
    reviewer: Option<String>,
    clear_reviewer: Option<bool>,
    outcome: Option<String>,
    parent: Option<String>,
    clear_parent: Option<bool>,
    /// Links to add; one already on the task changes nothing.
    add_evidence: Option<Vec<String>>,
    /// Your own words for this change, kept in the log.
    note: Option<String>,
}

impl TaskPatchInput {
    fn into_patch(self) -> FieldResult<TaskPatch> {
        Ok(TaskPatch {
            title: self.title,
            state: self.state.map(Into::into),
            owner: set_or_clear("owner", "clearOwner", self.owner, self.clear_owner)?,
            reviewer: set_or_clear("reviewer", "clearReviewer", self.reviewer, self.clear_reviewer)?,
            outcome: self.outcome,
            parent: set_or_clear("parent", "clearParent", self.parent, self.clear_parent)?,
            add_evidence: self.add_evidence.unwrap_or_default(),
            note: self.note,
        })
    }
}

fn set_or_clear(
    field: &str,
    clear_field: &str,
    value: Option<String>,
    clear: Option<bool>,
) -> FieldResult<Option<Option<String>>> {
    match (value, clear.unwrap_or(false)) {
        (Some(_), true) => Err(field_error(
            format!("set {field} or {clear_field}, not both"),
            "INVALID",
            [],
        )),
        (Some(value), false) => Ok(Some(Some(value))),
        (None, true) => Ok(Some(None)),
        (None, false) => Ok(None),
    }
}

fn graphql_int(field: &str, value: u64) -> FieldResult<i32> {
    i32::try_from(value).map_err(|_| {
        field_error(format!("{field} {value} is larger than a GraphQL Int"), "OUT_OF_RANGE", [])
    })
}

fn field_error<const N: usize>(message: impl Into<String>, code: &str, extra: [(&str, Value); N]) -> FieldError {
    let mut extensions = Object::with_capacity(N + 1);
    extensions.add_field("code", Value::scalar(code.to_owned()));
    for (key, value) in extra {
        extensions.add_field(key, value);
    }
    FieldError::new(message.into(), Value::Object(extensions))
}

fn revision_value(revision: u64) -> Value {
    i32::try_from(revision).map_or_else(|_| Value::scalar(revision.to_string()), Value::scalar)
}

// The message is always the store's own text. The last arm is deliberately
// open, so a variant added to the store maps to TASK_ERROR instead of failing
// to compile.
fn task_error(error: TaskError) -> FieldError {
    let message = error.to_string();
    match error {
        TaskError::NotFound(id) => field_error(message, "NOT_FOUND", [("id", Value::scalar(id))]),
        TaskError::Conflict { current_revision } => {
            field_error(message, "CONFLICT", [("currentRevision", revision_value(current_revision))])
        }
        TaskError::MutationReused { mutation_id } => {
            field_error(message, "MUTATION_REUSED", [("mutationId", Value::scalar(mutation_id))])
        }
        TaskError::Unreadable { file, .. } => field_error(message, "UNREADABLE", [("file", Value::scalar(file))]),
        TaskError::Invalid(_) => field_error(message, "INVALID", []),
        TaskError::StoreOwned => field_error(message, "STORE_OWNED", []),
        other => field_error(message, if matches!(other, TaskError::Io(_)) { "IO" } else { "TASK_ERROR" }, []),
    }
}

fn error_result(message: &str, code: &str) -> Json {
    json!({ "errors": [{ "message": message, "extensions": { "code": code } }] })
}

/// Lists confirmed current task results for a caller that cannot see the
/// response: each task's id, its revision as the store returned it, and the
/// mutationId that confirmed it.
pub fn describe_confirmed(confirmed: &[Confirmed]) -> String {
    confirmed
        .iter()
        .map(|result| match &result.mutation_id {
            Some(mutation_id) => format!("{} at revision {} (mutationId {mutation_id})", result.id, result.revision),
            None => format!(
                "{} at revision {} (no mutationId: read the task rather than repeating the change)",
                result.id, result.revision
            ),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn over_cap_result(cap: usize, confirmed: &[Confirmed], not_run: bool) -> Json {
    let mut message = format!(
        "the response would be larger than the {} KiB output cap; request fewer fields or a smaller page (limit), or split the request",
        cap / 1024
    );
    if !confirmed.is_empty() {
        message.push_str(&format!(
            ". Confirmed current task results (including replays): {}. A replayed mutationId reports the task as it is now, not as that change left it; repeating a change with its mutationId changes nothing again",
            describe_confirmed(confirmed)
        ));
    }
    if not_run {
        message.push_str(". Mutations after the cap was reached were not run");
    }
    json!({
        "data": null,
        "errors": [{
            "message": message,
            "extensions": { "code": "OUTPUT_TOO_LARGE", "capBytes": cap, "confirmed": confirmed },
        }],
    })
}

// Juniper 0.17.1 does not reject an operation whose root type the schema
// lacks: a mutation against a schema without one passes validation and then
// panics in the executor ("No mutation type found"). This asks juniper's own
// parser (run in `run`, after the lexical bounds hold) and operation selector
// which operation would run, so the read-only root fails cleanly. Read-only
// stays structural: with no mutation type there is nothing a mutation could
// resolve. Both helpers are `#[doc(hidden)]` juniper API, which is why
// Cargo.toml pins juniper exactly.
fn selects_mutation(document: &Document<'_, DefaultScalarValue>, operation_name: Option<&str>) -> bool {
    juniper::executor::get_operation(document, operation_name)
        .is_ok_and(|operation| operation.item.operation_type == OperationType::Mutation)
}

// Juniper resolves a field once per occurrence and merges the results, but
// its look-ahead, which the output meter reads to charge response keys before
// an object or list is built, finds only the first occurrence of a response
// name in a selection set. A second occurrence of an object field there could
// build keys the meter never saw, so each selection set may hold an object
// field (one with its own selection) once per response name. A fragment or
// inline fragment is its own selection set, and its fields are found there.
fn check_repeated_object_fields(document: &Document<'_, DefaultScalarValue>) -> Result<(), String> {
    fn check(selections: &[Selection<'_, DefaultScalarValue>]) -> Result<(), String> {
        let mut names = HashSet::new();
        for selection in selections {
            match selection {
                Selection::Field(field) => {
                    let field = &field.item;
                    if let Some(children) = &field.selection_set {
                        let name = field.alias.as_ref().unwrap_or(&field.name).item;
                        if !names.insert(name) {
                            return Err(format!(
                                "the query selects `{name}` more than once in one selection; select it once with every field you need, or give each occurrence its own alias"
                            ));
                        }
                        check(children)?;
                    }
                }
                Selection::InlineFragment(inline) => check(&inline.item.selection_set)?,
                Selection::FragmentSpread(_) => {}
            }
        }
        Ok(())
    }
    document.iter().try_for_each(|definition| match definition {
        juniper::Definition::Operation(operation) => check(&operation.item.selection_set),
        juniper::Definition::Fragment(fragment) => check(&fragment.item.selection_set),
    })
}

/// Lexical bounds on the raw query, checked before juniper's recursive
/// parser and validator run. String literals and comments are skipped.
fn check_document_shape(query: &str) -> Result<(), String> {
    #[derive(Default)]
    struct Definition {
        fragment: Option<String>,
        is_fragment: bool,
        names: usize,
        spreads: Vec<String>,
    }

    let bytes = query.as_bytes();
    let (mut at, mut depth, mut spreads, mut fragments) = (0, 0_usize, 0_usize, 0_usize);
    let (mut after_spread, mut expect_fragment_name) = (false, false);
    let mut definitions = Vec::new();
    let mut current: Option<Definition> = None;
    while at < bytes.len() {
        match bytes[at] {
            b'#' => {
                while at < bytes.len() && !matches!(bytes[at], b'\n' | b'\r') {
                    at += 1;
                }
                continue;
            }
            b'"' => {
                after_spread = false;
                at = skip_string(bytes, at);
                continue;
            }
            b'{' | b'(' | b'[' => {
                after_spread = false;
                depth += 1;
                if depth > NESTING_MAX {
                    return Err(format!(
                        "the query nests braces, parentheses or brackets more than {NESTING_MAX} levels deep; task queries need only a few"
                    ));
                }
                current.get_or_insert_with(Definition::default);
            }
            closing @ (b'}' | b')' | b']') => {
                after_spread = false;
                depth = depth.saturating_sub(1);
                if depth == 0 && closing == b'}' {
                    definitions.extend(current.take());
                }
            }
            b'.' if bytes[at..].starts_with(b"...") => {
                spreads += 1;
                if spreads > SPREADS_MAX {
                    return Err(format!("the query uses more than {SPREADS_MAX} fragment spreads (`...`)"));
                }
                after_spread = true;
                at += 3;
                continue;
            }
            b'_' | b'A'..=b'Z' | b'a'..=b'z' => {
                let start = at;
                while at < bytes.len() && (bytes[at] == b'_' || bytes[at].is_ascii_alphanumeric()) {
                    at += 1;
                }
                let name = &query[start..at];
                let definition = current.get_or_insert_with(Definition::default);
                if depth == 0 && definition.names == 0 && name == "fragment" {
                    fragments += 1;
                    if fragments > FRAGMENTS_MAX {
                        return Err(format!("the query defines more than {FRAGMENTS_MAX} fragments"));
                    }
                    definition.is_fragment = true;
                    expect_fragment_name = true;
                } else if expect_fragment_name {
                    definition.fragment = Some(name.to_owned());
                    expect_fragment_name = false;
                } else if after_spread && name != "on" {
                    definition.spreads.push(name.to_owned());
                }
                definition.names += 1;
                after_spread = false;
                continue;
            }
            byte if byte.is_ascii_whitespace() || byte == b',' => {}
            _ => after_spread = false,
        }
        at += 1;
    }
    definitions.extend(current);

    // A repeated fragment name is refused here. Juniper's cycle rule merges the
    // spreads of every definition sharing a name before it reports the
    // duplicate, so keeping only the first body would undercount its work.
    let mut bodies = HashMap::new();
    for definition in definitions.iter().filter(|definition| definition.is_fragment) {
        if let Some(name) = &definition.fragment {
            if bodies.insert(name.as_str(), definition).is_some() {
                return Err(format!("fragment {name} is defined more than once"));
            }
        }
    }
    // Juniper validates every definition, used or not, and its fragment-cycle
    // rule walks each fragment's spreads again along every path, without
    // remembering paths it has already walked. So every definition counts
    // here, operations and fragments alike, each with its fragments expanded.
    let mut memo = HashMap::new();
    let mut total = 0_usize;
    for definition in &definitions {
        let size = expanded_names(definition.names, &definition.spreads, &bodies, &mut memo, &mut HashSet::new())?;
        total = total.saturating_add(size);
    }
    if total > EXPANDED_NAMES_MAX {
        return Err(format!(
            "the query grows past {EXPANDED_NAMES_MAX} names once its fragments are expanded, counting every definition whether or not it is used; select fewer fields, use fewer fragment spreads or remove unused fragments"
        ));
    }
    return Ok(());

    // Names in a definition plus, recursively, those of the fragments it
    // spreads. Unknown fragments add nothing here; juniper's validation
    // rejects them. A cycle is refused: juniper rejects it too, and refusing
    // it here keeps every memoized size exact.
    fn expanded_names<'a>(
        names: usize,
        spreads: &'a [String],
        bodies: &HashMap<&'a str, &'a Definition>,
        memo: &mut HashMap<&'a str, usize>,
        visiting: &mut HashSet<&'a str>,
    ) -> Result<usize, String> {
        let mut total = names;
        for spread in spreads {
            let spread = spread.as_str();
            let size = if let Some(size) = memo.get(spread) {
                *size
            } else if let Some(body) = bodies.get(spread) {
                if !visiting.insert(spread) {
                    return Err(format!(
                        "fragment {spread} spreads itself, directly or through other fragments; fragments cannot form a cycle"
                    ));
                }
                let size = expanded_names(body.names, &body.spreads, bodies, memo, visiting)?;
                visiting.remove(spread);
                memo.insert(spread, size);
                size
            } else {
                0
            };
            total = total.saturating_add(size);
        }
        Ok(total)
    }
}

// Index just past the string starting at `start`, read as juniper 0.17.1
// lexes strings: one line, backslash escapes, and no block strings (`"""` is
// an empty string followed by a new one). Where juniper's lexer would fail,
// its parse stops there too, so skipping the rest of that line is safe.
fn skip_string(bytes: &[u8], start: usize) -> usize {
    let mut at = start + 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'"' => return at + 1,
            b'\n' | b'\r' => return at,
            _ => at += 1,
        }
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Arc<TaskStore>) {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(TaskStore::open(&root.path().join("tasks")).unwrap());
        (root, store)
    }

    fn agent(store: &Arc<TaskStore>, query: &str, variables: Json) -> TaskOutcome {
        execute_agent(Arc::clone(store), "Claude (session test)", None, query, Some(&variables), None)
    }

    fn create(store: &Arc<TaskStore>, title: &str) -> Json {
        let outcome = agent(
            store,
            "mutation($input: NewTaskInput!) { createTask(input: $input) { id revision } }",
            json!({ "input": { "title": title, "request": format!("please {title}"), "source": "room 1" } }),
        );
        assert!(outcome.errors().is_empty(), "{}", outcome.result);
        outcome.result["data"]["createTask"].clone()
    }

    fn code(outcome: &TaskOutcome) -> &str {
        outcome.errors()[0]["extensions"]["code"].as_str().unwrap()
    }

    #[test]
    fn create_then_query_back_keeps_the_request_verbatim() {
        let (_root, store) = store();
        let request = "  Направи задачите първо.\r\nThen move Claude & Codex → \"Experimental\".\n\ttabs, \\backslashes\\, emoji 🚀, 中文\n\n  trailing  ";
        let created = agent(
            &store,
            "mutation Make($input: NewTaskInput!) { createTask(input: $input, mutationId: \"m-1\") { id state revision request log { by what mutationId } } }",
            json!({ "input": {
                "title": "Task management first", "request": request, "source": "room 444, 2026-10-14T19:48Z",
                "owner": "claude", "evidence": ["commit dba7a90"],
            } }),
        );
        assert!(created.errors().is_empty(), "{}", created.result);
        let task = &created.result["data"]["createTask"];
        assert_eq!(task["id"], "T-1");
        assert_eq!(task["state"], "open");
        assert_eq!(task["revision"], 1);
        assert_eq!(task["request"], request);
        assert_eq!(task["log"], json!([{ "by": "Claude (session test)", "what": "created", "mutationId": "m-1" }]));
        assert_eq!(created.committed, [Confirmed { id: "T-1".into(), revision: 1, mutation_id: Some("m-1".into()) }]);

        let read = execute_read_only(
            Arc::clone(&store),
            "query($id: String!) { task(id: $id) { request owner evidence parent } tasks(state: open) { total items { id title } unreadable { file } } }",
            Some(&json!({ "id": "T-1" })),
            None,
        );
        assert!(read.errors().is_empty(), "{}", read.result);
        let data = &read.result["data"];
        assert_eq!(data["task"]["request"].as_str().unwrap().as_bytes(), request.as_bytes());
        assert_eq!(data["task"]["owner"], "claude");
        assert_eq!(data["task"]["evidence"], json!(["commit dba7a90"]));
        assert_eq!(data["task"]["parent"], Json::Null);
        assert_eq!(data["tasks"], json!({ "total": 1, "items": [{ "id": "T-1", "title": "Task management first" }], "unreadable": [] }));
        assert_eq!(store.get("T-1").unwrap().request.as_bytes(), request.as_bytes());
    }

    #[test]
    fn stale_expected_revision_returns_current_revision_and_writes_nothing() {
        let (_root, store) = store();
        create(&store, "contested");
        let update = "mutation($rev: Int!, $patch: TaskPatchInput!) { updateTask(id: \"T-1\", expectedRevision: $rev, patch: $patch) { revision state owner log { what note } } }";
        let first = agent(&store, update, json!({ "rev": 1, "patch": { "state": "doing", "owner": "codex", "note": "taking it" } }));
        assert!(first.errors().is_empty(), "{}", first.result);
        assert_eq!(first.result["data"]["updateTask"]["revision"], 2);
        assert_eq!(first.result["data"]["updateTask"]["log"][1], json!({ "what": "state open -> doing; owner (none) -> codex", "note": "taking it" }));

        let stale = agent(&store, update, json!({ "rev": 1, "patch": { "state": "blocked" } }));
        assert_eq!(code(&stale), "CONFLICT");
        assert_eq!(stale.errors()[0]["extensions"]["currentRevision"], 2);
        assert!(stale.first_error_message().unwrap().contains("revision 2"));
        assert_eq!(stale.result["data"]["updateTask"], Json::Null);
        assert!(stale.committed.is_empty());
        assert_eq!(store.get("T-1").unwrap().revision, 2);

        let cleared = agent(&store, update, json!({ "rev": 2, "patch": { "clearOwner": true } }));
        assert_eq!(cleared.result["data"]["updateTask"]["owner"], Json::Null, "{}", cleared.result);
        let both = agent(&store, update, json!({ "rev": 3, "patch": { "owner": "x", "clearOwner": true } }));
        assert_eq!(code(&both), "INVALID");
        // There is no way to send request or source in a patch.
        let forged = agent(&store, update, json!({ "rev": 3, "patch": { "request": "rewritten" } }));
        assert!(forged.first_error_message().unwrap().contains("request"), "{}", forged.result);
        assert_eq!(store.get("T-1").unwrap().request, "please contested");
    }

    #[test]
    fn mutation_id_replay_returns_the_same_task_and_reuse_names_the_id() {
        let (_root, store) = store();
        let create = "mutation($input: NewTaskInput!) { createTask(input: $input, mutationId: \"m-7\") { id revision } }";
        let input = json!({ "input": { "title": "once", "request": "do it once", "source": "room 2" } });
        let first = agent(&store, create, input.clone());
        let again = agent(&store, create, input);
        assert_eq!(first.result, again.result);
        assert_eq!(again.result["data"]["createTask"]["id"], "T-1");
        assert_eq!(store.list(&TaskFilter::default(), 0, 10).unwrap().total, 1);

        let other = agent(&store, create, json!({ "input": { "title": "other", "request": "x", "source": "y" } }));
        assert_eq!(code(&other), "MUTATION_REUSED");
        assert_eq!(other.errors()[0]["extensions"]["mutationId"], "m-7");
        assert!(other.first_error_message().unwrap().contains("m-7"));
    }

    #[test]
    fn read_only_root_has_no_mutations() {
        let (_root, store) = store();
        create(&store, "kept");
        for query in [
            "mutation { createTask(input: { title: \"t\", request: \"r\", source: \"s\" }) { id } }",
            "mutation { updateTask(id: \"T-1\", expectedRevision: 1, patch: { state: done }) { id } }",
        ] {
            let outcome = execute_read_only(Arc::clone(&store), query, None, None);
            assert!(!outcome.errors().is_empty(), "{}", outcome.result);
            assert_eq!(code(&outcome), "READ_ONLY", "{}", outcome.result);
            assert!(outcome.result.get("data").is_none(), "{}", outcome.result);
            assert!(outcome.committed.is_empty());
        }
        let task = store.get("T-1").unwrap();
        assert_eq!((task.revision, task.state), (1, TaskState::Open));
        assert_eq!(store.list(&TaskFilter::default(), 0, 10).unwrap().total, 1);
    }

    #[test]
    fn resolver_budget_allows_twenty_and_refuses_the_twenty_first() {
        let (_root, store) = store();
        create(&store, "budget");
        let fields = |count: usize| (0..count).map(|n| format!("a{n}: task(id: \"T-1\") {{ id }}")).collect::<Vec<_>>().join(" ");
        let twenty = agent(&store, &format!("{{ {} }}", fields(20)), json!({}));
        assert!(twenty.errors().is_empty(), "{}", twenty.result);
        let over = agent(&store, &format!("{{ {} }}", fields(21)), json!({}));
        assert_eq!(over.errors().len(), 1, "{}", over.result);
        assert_eq!(code(&over), "BUDGET_EXCEEDED");
        assert_eq!(over.result["data"]["a19"]["id"], "T-1");
        assert_eq!(over.result["data"]["a20"], Json::Null);

        // Mutations draw from the same budget.
        let creates = (0..21)
            .map(|n| format!("c{n}: createTask(input: {{ title: \"t{n}\", request: \"r\", source: \"s\" }}) {{ id }}"))
            .collect::<Vec<_>>()
            .join(" ");
        let over = agent(&store, &format!("mutation {{ {creates} }}"), json!({}));
        assert_eq!(code(&over), "BUDGET_EXCEEDED");
        assert_eq!(over.committed.len(), 20);
        assert_eq!(store.list(&TaskFilter::default(), 0, 100).unwrap().total, 21);
    }

    #[test]
    fn limit_above_two_hundred_is_rejected_not_clamped() {
        let (_root, store) = store();
        create(&store, "page");
        let rejected = agent(&store, "{ tasks(limit: 201) { total } }", json!({}));
        assert_eq!(code(&rejected), "INVALID");
        assert!(rejected.first_error_message().unwrap().contains("0 to 200"));
        let negative = agent(&store, "{ tasks(offset: -1) { total } }", json!({}));
        assert_eq!(code(&negative), "INVALID");
        let max = agent(&store, "{ tasks(limit: 200) { total items { id } } }", json!({}));
        assert_eq!(max.result["data"]["tasks"], json!({ "total": 1, "items": [{ "id": "T-1" }] }));
        let empty = agent(&store, "{ tasks(limit: 0) { total items { id } } }", json!({}));
        assert_eq!(empty.result["data"]["tasks"], json!({ "total": 1, "items": [] }));
    }

    #[test]
    fn output_cap_replaces_the_response_and_still_names_confirmed_results() {
        let (_root, store) = store();
        let big = "x".repeat(4096);
        for n in 0..3 {
            let outcome = agent(
                &store,
                "mutation($input: NewTaskInput!) { createTask(input: $input) { id } }",
                json!({ "input": { "title": format!("big {n}"), "request": big, "source": "s" } }),
            );
            assert!(outcome.errors().is_empty());
        }
        let run_with_cap = |cap: usize, query: &str| {
            let context = TaskContext::new(Arc::clone(&store), Some("tester".into()), None, cap);
            run(&AGENT_ROOT, context, query, None, None)
        };
        let small = run_with_cap(10 * 1024, "{ tasks { items { id request } } }");
        assert_eq!(code(&small), "OUTPUT_TOO_LARGE", "{}", small.result);
        assert_eq!(small.result["data"], Json::Null);
        assert!(small.first_error_message().unwrap().contains("10 KiB"));
        // Aliases multiply output; the meter stops them without building it.
        let aliases = (0..200).map(|n| format!("r{n}: request")).collect::<Vec<_>>().join(" ");
        let fanned = run_with_cap(64 * 1024, &format!("{{ tasks {{ items {{ {aliases} }} }} }}"));
        assert_eq!(code(&fanned), "OUTPUT_TOO_LARGE");
        let fits = run_with_cap(64 * 1024, "{ tasks { items { id title } } }");
        assert!(fits.errors().is_empty(), "{}", fits.result);

        let confirmed = run_with_cap(
            2 * 1024,
            "mutation { a: updateTask(id: \"T-1\", expectedRevision: 1, patch: { state: doing }, mutationId: \"m-a\") { request } \
             b: updateTask(id: \"T-2\", expectedRevision: 1, patch: { state: doing }) { request } \
             c: updateTask(id: \"T-3\", expectedRevision: 1, patch: { state: doing }) { id } }",
        );
        assert_eq!(code(&confirmed), "OUTPUT_TOO_LARGE", "{}", confirmed.result);
        assert_eq!(
            confirmed.errors()[0]["extensions"]["confirmed"],
            json!([{ "id": "T-1", "revision": 2, "mutationId": "m-a" }])
        );
        let message = confirmed.first_error_message().unwrap();
        assert!(message.contains("T-1 at revision 2 (mutationId m-a)") && message.contains("were not run"), "{message}");
        assert_eq!(store.get("T-1").unwrap().revision, 2);
        assert_eq!(store.get("T-2").unwrap().revision, 1);
        assert_eq!(store.get("T-3").unwrap().revision, 1);
    }

    #[test]
    fn mutation_guard_runs_before_every_change() {
        let (_root, store) = store();
        let refuse: MutationGuard = Box::new(|| Err("sideband caller run is stale".into()));
        let outcome = execute_agent(
            Arc::clone(&store),
            "agent",
            Some(refuse),
            "mutation { createTask(input: { title: \"t\", request: \"r\", source: \"s\" }) { id } }",
            None,
            None,
        );
        assert_eq!(code(&outcome), "CALLER_STALE");
        assert!(outcome.first_error_message().unwrap().contains("nothing was changed"));
        assert_eq!(store.list(&TaskFilter::default(), 0, 10).unwrap().total, 0);
        // Queries do not consult the guard.
        let never: MutationGuard = Box::new(|| panic!("queries never call the guard"));
        let read = execute_agent(Arc::clone(&store), "agent", Some(never), "{ tasks { total } }", None, None);
        assert!(read.errors().is_empty(), "{}", read.result);
    }

    #[test]
    fn lexical_bounds_reject_bombs_before_the_parser_runs() {
        let (_root, store) = store();
        LazyLock::force(&AGENT_ROOT);
        LazyLock::force(&READ_ONLY_ROOT);
        // 16 KiB of `{a` would need a deep recursive parse; on a 256 KiB stack
        // it must be refused by the iterative scan instead.
        let bomb = "{a".repeat(QUERY_MAX_BYTES / 2);
        assert_eq!(bomb.len(), QUERY_MAX_BYTES);
        let (agent_store, read_store, query) = (Arc::clone(&store), Arc::clone(&store), bomb.clone());
        let outcomes = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                (
                    execute_agent(agent_store, "agent", None, &query, None, None),
                    execute_read_only(read_store, &query, None, None),
                )
            })
            .unwrap()
            .join()
            .unwrap();
        for outcome in [outcomes.0, outcomes.1] {
            assert_eq!(code(&outcome), "QUERY_TOO_COMPLEX");
            assert!(outcome.first_error_message().unwrap().contains("12 levels"));
        }
        for list_bomb in ["{ task(id: [[[[[[[[[[[[[\"T-1\"]]]]]]]]]]]]]) { id } }", "query(".repeat(13).as_str()] {
            assert_eq!(code(&agent(&store, list_bomb, json!({}))), "QUERY_TOO_COMPLEX");
        }

        // Brackets inside strings (escaped quotes included) and comments do
        // not count.
        let quoted = format!(
            "# {{{{{{{{{{{{{{{{\n{{ tasks(owner: \"{}\", parent: \"\\\"{}\") {{ total }} }}",
            "{".repeat(40),
            "[".repeat(40)
        );
        let fine = agent(&store, &quoted, json!({}));
        assert!(fine.errors().is_empty(), "{}", fine.result);
        // Juniper has no block strings: `"""` is "" and then a new string, so
        // what follows `" "` is code to its parser and is counted here too.
        let hidden = format!("{{ tasks(owner: [\"\"\" \" {}\"\"\"]) {{ total }} }}", "{a:".repeat(40));
        assert_eq!(code(&agent(&store, &hidden, json!({}))), "QUERY_TOO_COMPLEX");
        let normal = agent(&store, "query List { tasks(state: open, limit: 5) { total items { id log { at } } } }", json!({}));
        assert!(normal.errors().is_empty(), "{}", normal.result);

        let spreads = format!("{{ tasks {{ items {{ {} }} }} }} fragment F on Task {{ id }}", "...F ".repeat(33));
        assert!(agent(&store, &spreads, json!({})).first_error_message().unwrap().contains("32 fragment spreads"));
        let definitions = (0..17).map(|n| format!("fragment F{n} on Task {{ id }}")).collect::<Vec<_>>().join(" ");
        let many = agent(&store, &format!("{{ tasks {{ items {{ ...F0 }} }} }} {definitions}"), json!({}));
        assert!(many.first_error_message().unwrap().contains("16 fragments"));
        // 15 fragments that each spread the next one twice: 2^15 expansions.
        let chain = (0..15)
            .map(|n| format!("fragment F{n} on Task {{ ...F{m} ...F{m} }}", m = n + 1))
            .collect::<Vec<_>>()
            .join(" ");
        let exponential = agent(&store, &format!("{{ tasks {{ items {{ ...F0 }} }} }} {chain} fragment F15 on Task {{ id title }}"), json!({}));
        assert!(exponential.first_error_message().unwrap().contains("fragments are expanded"), "{}", exponential.result);
        let used = agent(&store, "{ tasks { items { ...Brief } } } fragment Brief on Task { id ... on Task { title } }", json!({}));
        assert!(used.errors().is_empty(), "{}", used.result);
    }

    #[test]
    fn introspection_is_disabled_and_request_bounds_hold() {
        let (_root, store) = store();
        let introspection = agent(&store, "{ __schema { types { name } } }", json!({}));
        assert!(introspection.first_error_message().unwrap().contains("introspection"), "{}", introspection.result);
        let typename = agent(&store, "{ __typename }", json!({}));
        assert_eq!(typename.result["data"]["__typename"], "Query");

        assert_eq!(code(&agent(&store, &format!("{{ tasks {{ total }} }}{}", " ".repeat(QUERY_MAX_BYTES)), json!({}))), "BAD_REQUEST");
        assert_eq!(code(&agent(&store, "{ tasks { total } }", json!({ "big": "x".repeat(VARIABLES_MAX_BYTES) }))), "BAD_REQUEST");
        assert_eq!(code(&agent(&store, "{ tasks { total } }", json!(["not", "an", "object"]))), "BAD_REQUEST");
        let long_name = "a".repeat(OPERATION_NAME_MAX_CHARS + 1);
        let outcome = execute_agent(Arc::clone(&store), "agent", None, "{ tasks { total } }", None, Some(&long_name));
        assert_eq!(code(&outcome), "BAD_REQUEST");
        assert!(check_request("{ tasks { total } }", Some(&json!({ "a": 1 })), Some(&"é".repeat(128))).is_ok());
    }

    #[test]
    fn task_errors_map_to_coded_field_errors() {
        let (_root, store) = store();
        let missing = agent(&store, "{ task(id: \"T-9\") { id } }", json!({}));
        assert_eq!(code(&missing), "NOT_FOUND");
        assert_eq!(missing.errors()[0]["extensions"]["id"], "T-9");
        assert_eq!(missing.first_error_message(), Some("task T-9 does not exist"));
        let invalid = agent(&store, "mutation { createTask(input: { title: \" \", request: \"r\", source: \"s\" }) { id } }", json!({}));
        assert_eq!(code(&invalid), "INVALID");
        assert!(invalid.committed.is_empty());
    }

    #[test]
    fn no_task_folder_is_a_plain_graphql_error() {
        let outcome = no_task_folder(NO_TASK_FOLDER);
        assert_eq!(
            outcome.result,
            json!({ "errors": [{ "message": "No task folder is set. Choose one in PRIM-1 Settings → Task folder.", "extensions": { "code": "NO_TASK_FOLDER" } }] })
        );
    }

    /// A task whose log holds 128 short entries, written directly to its file.
    fn task_with_long_log(root: &tempfile::TempDir, store: &Arc<TaskStore>) {
        create(store, "t");
        let path = root.path().join("tasks").join("T-1.json");
        let mut record: Json = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut entry = record["log"][0].clone();
        entry["what"] = json!("note");
        for _ in 1..128 {
            record["log"].as_array_mut().unwrap().push(entry.clone());
        }
        record["revision"] = json!(128);
        std::fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() < 64 * 1024);
    }

    // Codex review of ff8985e, finding 1: a 9,000-character alias over 128 log
    // entries is 1.15 MB of keys. The meter must charge them before the list
    // is built, not after.
    #[test]
    fn long_alias_over_a_log_is_refused_by_the_meter_before_it_is_built() {
        let (root, store) = store();
        task_with_long_log(&root, &store);
        let alias = "a".repeat(9_000);
        let query = format!("{{ task(id: \"T-1\") {{ log {{ {alias}: what }} }} }}");
        assert!(check_request(&query, None, None).is_ok() && check_document_shape(&query).is_ok());

        let context = TaskContext::new(Arc::clone(&store), Some("tester".into()), None, OUTPUT_MAX_BYTES);
        let response = GraphQLResponse::from_result(juniper::execute_sync(&query, None, &AGENT_ROOT, &Variables::new(), &context));
        assert!(context.over_cap.load(Ordering::Relaxed), "charged {}", context.charged.load(Ordering::Relaxed));
        assert!(context.charged.load(Ordering::Relaxed) > OUTPUT_MAX_BYTES);
        // Nothing was built: the refused list resolved empty.
        let built = serde_json::to_value(&response).unwrap();
        assert_eq!(built["data"]["task"]["log"], json!([]), "{built}");
        assert_eq!(code(&agent(&store, &query, json!({}))), "OUTPUT_TOO_LARGE");

        // A short alias is charged at its real length and fits.
        let fits = agent(&store, "{ task(id: \"T-1\") { log { w: what } } }", json!({}));
        assert!(fits.errors().is_empty(), "{}", fits.result);
        assert_eq!(fits.result["data"]["task"]["log"].as_array().unwrap().len(), 128);

        // Each fragment is its own selection set, so each occurrence of `log`
        // is charged with its own keys.
        let fragments = format!(
            "{{ task(id: \"T-1\") {{ ...Short ...Long }} }} fragment Short on Task {{ log {{ at }} }} fragment Long on Task {{ log {{ {alias}: what }} }}"
        );
        assert_eq!(code(&agent(&store, &fragments, json!({}))), "OUTPUT_TOO_LARGE");
        // The meter reads the first occurrence of a response name in one
        // selection set, so an object field may appear there only once.
        let repeated = agent(&store, &format!("{{ task(id: \"T-1\") {{ log {{ at }} log {{ {alias}: what }} }} }}"), json!({}));
        assert_eq!(code(&repeated), "QUERY_TOO_COMPLEX", "{}", repeated.result);
        assert!(repeated.first_error_message().unwrap().contains("`log` more than once"));
        let roots = agent(&store, "{ task(id: \"T-1\") { id } task(id: \"T-1\") { title } }", json!({}));
        assert_eq!(code(&roots), "QUERY_TOO_COMPLEX", "{}", roots.result);
        // Repeated leaf fields and separately aliased object fields are fine.
        let fine = agent(&store, "{ a: task(id: \"T-1\") { id id log { at } } b: task(id: \"T-1\") { log { at } } }", json!({}));
        assert!(fine.errors().is_empty(), "{}", fine.result);
    }

    // Codex review of ff8985e, finding 2: a root field spends its unit before
    // anything that can fail, so failures count toward the twenty.
    #[test]
    fn invalid_updates_spend_the_root_resolver_budget() {
        let (_root, store) = store();
        let bad_revision = (0..10).map(|i| format!("u{i}: updateTask(id: \"T-1\", expectedRevision: -1, patch: {{state: doing}}) {{id}}"));
        let bad_patch = (10..20)
            .map(|i| format!("u{i}: updateTask(id: \"T-1\", expectedRevision: 1, patch: {{owner: \"x\", clearOwner: true}}) {{id}}"));
        let bad = bad_revision.chain(bad_patch).collect::<Vec<_>>().join(" ");
        let query = format!("mutation {{ {bad} after: createTask(input: {{title: \"t\", request: \"r\", source: \"s\"}}) {{id}} }}");
        let outcome = agent(&store, &query, json!({}));
        assert_eq!(store.list(&TaskFilter::default(), 0, 10).unwrap().total, 0, "{}", outcome.result);
        assert!(outcome.committed.is_empty());
        let codes = outcome.errors().iter().map(|error| error["extensions"]["code"].as_str().unwrap()).collect::<Vec<_>>();
        assert_eq!(codes.iter().filter(|code| **code == "INVALID").count(), 20, "{codes:?}");
        assert_eq!(codes.last(), Some(&"BUDGET_EXCEEDED"), "{codes:?}");
        assert_eq!(outcome.result["data"]["after"], Json::Null);

        // Failed queries count too.
        let missing = (0..20).map(|n| format!("m{n}: task(id: \"T-9\") {{ id }}")).collect::<Vec<_>>().join(" ");
        let outcome = agent(&store, &format!("{{ {missing} last: tasks {{ total }} }}"), json!({}));
        let codes = outcome.errors().iter().map(|error| error["extensions"]["code"].as_str().unwrap()).collect::<Vec<_>>();
        assert_eq!(codes.iter().filter(|code| **code == "NOT_FOUND").count(), 20, "{codes:?}");
        assert_eq!(codes.last(), Some(&"BUDGET_EXCEEDED"), "{codes:?}");
        assert_eq!(outcome.result["data"]["last"], Json::Null);
    }

    // Codex review of ff8985e, finding 3: juniper validates unused fragments
    // too, so every definition counts toward the expansion bound.
    #[test]
    fn unused_fragment_expansion_is_bounded_before_validation() {
        let (_root, store) = store();
        let chain = (0..15)
            .map(|i| format!("fragment F{i} on Task {{ ...F{next} ...F{next} }}", next = i + 1))
            .collect::<Vec<_>>()
            .join(" ");
        let query = format!("{{ __typename }} {chain} fragment F15 on Task {{ id title }}");
        assert!(check_request(&query, None, None).is_ok());
        let refused = check_document_shape(&query).unwrap_err();
        assert!(refused.contains("fragments are expanded") && refused.contains("whether or not it is used"), "{refused}");
        assert_eq!(code(&agent(&store, &query, json!({}))), "QUERY_TOO_COMPLEX");

        // A cycle, used or not, is refused before juniper walks it.
        for cycle in [
            "{ __typename } fragment A on Task { ...B } fragment B on Task { ...A }",
            "{ task(id: \"T-1\") { ...A } } fragment A on Task { id ...A }",
        ] {
            let refused = check_document_shape(cycle).unwrap_err();
            assert!(refused.contains("cannot form a cycle"), "{refused}");
        }
        // A short unused fragment passes the bounds; juniper then reports it.
        let unused = agent(&store, "{ __typename } fragment Brief on Task { id title }", json!({}));
        assert!(unused.first_error_message().unwrap().contains("Brief"), "{}", unused.result);
    }

    // Codex review of ff8985e, finding 4: a replayed mutationId returns the task
    // as it is now, so results are labelled as confirmed current results.
    #[test]
    fn replayed_mutation_is_reported_as_a_confirmed_current_result() {
        let (_root, store) = store();
        let big = "x".repeat(4096);
        let created = agent(
            &store,
            "mutation($input: NewTaskInput!) { createTask(input: $input) { id } }",
            json!({ "input": { "title": "replayed", "request": big, "source": "s" } }),
        );
        assert!(created.errors().is_empty(), "{}", created.result);
        let update = |revision: i32, state: &str, mutation_id: &str| {
            format!(
                "mutation {{ updateTask(id: \"T-1\", expectedRevision: {revision}, patch: {{ state: {state} }}, mutationId: \"{mutation_id}\") {{ revision request }} }}"
            )
        };
        assert!(agent(&store, &update(1, "doing", "m-1"), json!({})).errors().is_empty());
        assert!(agent(&store, &update(2, "review", "m-2"), json!({})).errors().is_empty());

        // Replaying m-1 changes nothing and confirms the task as it is now:
        // revision 3, not the revision 2 that m-1 produced.
        let context = TaskContext::new(Arc::clone(&store), Some("Claude (session test)".into()), None, 2 * 1024);
        let replay = run(&AGENT_ROOT, context, &update(1, "doing", "m-1"), None, None);
        assert_eq!(code(&replay), "OUTPUT_TOO_LARGE", "{}", replay.result);
        let confirmed = [Confirmed { id: "T-1".into(), revision: 3, mutation_id: Some("m-1".into()) }];
        assert_eq!(replay.committed, confirmed);
        assert_eq!(replay.errors()[0]["extensions"]["confirmed"], json!([{ "id": "T-1", "revision": 3, "mutationId": "m-1" }]));
        assert!(replay.errors()[0]["extensions"].get("committed").is_none());
        let message = replay.first_error_message().unwrap();
        assert!(
            message.contains("Confirmed current task results (including replays): T-1 at revision 3 (mutationId m-1)")
                && message.contains("as it is now"),
            "{message}"
        );
        assert!(!message.contains("committed"), "{message}");
        let task = store.get("T-1").unwrap();
        assert_eq!((task.revision, task.state, task.log.len()), (3, TaskState::Review, 3));

        assert_eq!(
            describe_confirmed(&[
                Confirmed { id: "T-1".into(), revision: 3, mutation_id: Some("m-1".into()) },
                Confirmed { id: "T-2".into(), revision: 1, mutation_id: None },
            ]),
            "T-1 at revision 3 (mutationId m-1), T-2 at revision 1 (no mutationId: read the task rather than repeating the change)"
        );
    }

    /// Pins the agent schema; the tool description summarizes it.
    #[test]
    fn agent_schema_sdl_is_pinned() {
        let sdl = AGENT_ROOT.as_sdl();
        println!("{sdl}");
        for expected in [
            "type Query {",
            "task(id: String!): Task",
            "tasks(state: TaskState, owner: String, parent: String, offset: Int! = 0, limit: Int! = 50): TaskPage",
            "createTask(input: NewTaskInput!, mutationId: String): Task",
            "updateTask(id: String!, expectedRevision: Int!, patch: TaskPatchInput!, mutationId: String): Task",
            "clearOwner: Boolean",
            "enum TaskState {",
        ] {
            assert!(sdl.contains(expected), "missing {expected:?} in\n{sdl}");
        }
        let patch = sdl.split("input TaskPatchInput {").nth(1).unwrap().split('}').next().unwrap();
        assert!(!patch.contains("request") && !patch.contains("source"), "{patch}");
        let read_only = READ_ONLY_ROOT.as_sdl();
        assert!(!read_only.contains("type Mutation") && !read_only.contains("createTask"), "{read_only}");
    }

    // Codex 535: two definitions of one fragment name must not pass the shape
    // check with only the first body counted.
    #[test]
    fn duplicate_fragment_names_are_refused_before_validation() {
        let (_dir, store) = store();
        let query = "{ __typename } fragment F on Query { __typename } fragment F on Query { ...F }";
        let outcome = agent(&store, query, json!({}));
        assert_eq!(code(&outcome), "QUERY_TOO_COMPLEX", "{}", outcome.result);
        assert!(outcome.result.to_string().contains("defined more than once"), "{}", outcome.result);
    }
}
