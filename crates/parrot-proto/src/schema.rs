//! The generated JSON Schema of the protocol (spec 38).
//!
//! `all()` builds the committed `spec/schema/parrot.schema.json`; add each new wire type with one
//! line in [`register`], and each method with one line in [`register`]'s method table.

use std::collections::BTreeMap;

use schemars::{JsonSchema, SchemaGenerator};
use serde_json::{Map, Value, json};

use crate::{
    action, agent, driver, driver_data, env, error, function, handshake, ids, item, link, option, principal,
    prompt, request, seed, session, state, subscribe, task, usage, workspace,
};

/// The schema of one type, with its own `$defs`.
pub fn schema_for<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schemas serialize")
}

/// A `$ref` into the document's own `$defs`.
fn ref_to<T: JsonSchema>() -> Value {
    json!({ "$ref": format!("#/$defs/{}", T::schema_name()) })
}

/// Every protocol type and method, one line each, grouped by module. The types agent and the
/// integrator extend these lists.
pub fn register(r: &mut Registry) {
    // envelope
    r.add::<envelope::Message>();
    r.add::<envelope::EnvelopeRequest>();
    r.add::<envelope::EnvelopeNotification>();
    r.add::<envelope::EnvelopeResponse>();
    r.add::<envelope::RpcError>();
    // error
    r.add::<error::UnsupportedData>();
    r.add::<error::ForbiddenData>();
    r.add::<error::WrongStateData>();
    r.add::<error::NoSharedVersionData>();
    r.add::<error::DetailData>();
    r.add::<error::MovedData>();
    r.add::<error::IsolationData>();
    // ids
    r.add::<ids::Empty>();
    // principal
    r.add::<principal::Scope>();
    r.add::<principal::Isolation>();
    // handshake
    r.add::<handshake::InitializeParams>();
    r.add::<handshake::InitializeResult>();
    r.add::<handshake::PeerInfo>();
    r.add::<handshake::PeerKind>();
    r.add::<handshake::RuntimeInfo>();
    r.add::<handshake::ConnectionGrant>();
    // env
    r.add::<env::Environment>();
    r.add::<env::Os>();
    r.add::<env::EnvStatus>();
    r.add::<env::EnvListResult>();
    // workspace
    r.add::<workspace::Workspace>();
    r.add::<workspace::WorkspaceKind>();
    r.add::<workspace::WorkspaceRequest>();
    r.add::<workspace::Carry>();
    r.add::<workspace::WorkspaceCreateParams>();
    r.add::<workspace::WorkspaceResult>();
    r.add::<workspace::WorkspaceListParams>();
    r.add::<workspace::WorkspaceListResult>();
    r.add::<workspace::WorkspaceStatusParams>();
    r.add::<workspace::WorkspaceStatusResult>();
    r.add::<workspace::WorkspaceMergeParams>();
    r.add::<workspace::MergeMethod>();
    r.add::<workspace::WorkspaceMergeResult>();
    r.add::<workspace::WorkspaceTransferParams>();
    r.add::<workspace::WorkspaceRemoveParams>();
    // agent
    r.add::<agent::Agent>();
    r.add::<agent::AgentDriver>();
    r.add::<agent::DriverKind>();
    r.add::<agent::HarnessInfo>();
    r.add::<agent::AuthStatus>();
    r.add::<agent::Control>();
    r.add::<agent::AgentListParams>();
    r.add::<agent::AgentListResult>();
    r.add::<agent::AgentRefreshParams>();
    r.add::<agent::AgentRefreshResult>();
    // session
    r.add::<session::Session>();
    r.add::<session::SessionKind>();
    r.add::<session::SessionParent>();
    r.add::<session::McpServer>();
    r.add::<session::McpStatus>();
    r.add::<session::TaskStatus>();
    r.add::<session::SessionSummary>();
    r.add::<session::Binding>();
    r.add::<session::BindingStatus>();
    r.add::<session::BindingVia>();
    r.add::<session::BindingPending>();
    r.add::<session::Ref>();
    r.add::<session::RefStrength>();
    r.add::<session::Background>();
    r.add::<session::BackgroundKind>();
    r.add::<session::OnParentEnd>();
    r.add::<session::SessionNewParams>();
    r.add::<session::SessionNewResult>();
    r.add::<session::SessionListParams>();
    r.add::<session::SessionListResult>();
    r.add::<session::SessionRef>();
    r.add::<session::SessionGetResult>();
    r.add::<session::SessionTreeParams>();
    r.add::<session::SessionTreeResult>();
    r.add::<session::TreeNode>();
    r.add::<session::SessionStartParams>();
    r.add::<session::SessionBindingResult>();
    r.add::<session::SessionStopParams>();
    r.add::<session::RebindRequire>();
    r.add::<session::SessionRebindParams>();
    r.add::<session::SessionRewindParams>();
    r.add::<session::SessionRenameParams>();
    r.add::<session::SessionArchiveParams>();
    r.add::<session::SessionDiscoverParams>();
    r.add::<session::SessionDiscoverResult>();
    r.add::<session::DiscoveredSession>();
    r.add::<session::SessionAdoptParams>();
    // subscribe
    r.add::<subscribe::Target>();
    r.add::<subscribe::SubscribeParams>();
    r.add::<subscribe::SubscribeResult>();
    r.add::<subscribe::Snapshot>();
    r.add::<subscribe::IndexSnapshot>();
    r.add::<subscribe::SessionSnapshot>();
    r.add::<subscribe::ConnectionInfo>();
    r.add::<subscribe::UnsubscribeParams>();
    r.add::<subscribe::UpdateParams>();
    r.add::<subscribe::Update>();
    r.add::<subscribe::NoticeLevel>();
    r.add::<subscribe::DeletedKind>();
    r.add::<subscribe::ReadDirection>();
    r.add::<subscribe::SessionReadParams>();
    r.add::<subscribe::SessionReadResult>();
    r.add::<subscribe::ItemGetParams>();
    r.add::<subscribe::ItemGetResult>();
    // state
    r.add::<state::State>();
    r.add::<state::Turn>();
    r.add::<state::TurnStatus>();
    r.add::<state::TurnStop>();
    r.add::<state::TurnError>();
    // item
    r.add::<item::ItemPatch>();
    r.add::<item::ItemKind>();
    r.add::<item::ItemSource>();
    r.add::<item::Origin>();
    r.add::<item::OriginKind>();
    r.add::<item::OriginCause>();
    r.add::<item::ItemCommon>();
    r.add::<item::ItemBody>();
    r.add::<item::UserItem>();
    r.add::<item::UserStatus>();
    r.add::<item::TextItem>();
    r.add::<item::ReasoningItem>();
    r.add::<item::ToolItem>();
    r.add::<item::ToolStatus>();
    r.add::<item::RunStatus>();
    r.add::<item::Caller>();
    r.add::<item::ScriptItem>();
    r.add::<item::CommandItem>();
    r.add::<item::FileChangeItem>();
    r.add::<item::FileChange>();
    r.add::<item::FileOp>();
    r.add::<item::PlanItem>();
    r.add::<item::PlanEntry>();
    r.add::<item::PlanStatus>();
    r.add::<item::SubagentItem>();
    r.add::<item::WebItem>();
    r.add::<item::ReviewItem>();
    r.add::<item::CompactionItem>();
    r.add::<item::CompactionTrigger>();
    r.add::<item::HookItem>();
    r.add::<item::HookOutcome>();
    r.add::<item::HandoffItem>();
    r.add::<item::ErrorItem>();
    // prompt
    r.add::<prompt::SessionPromptParams>();
    r.add::<prompt::ContextEntry>();
    r.add::<prompt::Attachment>();
    r.add::<prompt::Delivery>();
    r.add::<prompt::SessionPromptResult>();
    r.add::<prompt::PromptDisposition>();
    r.add::<prompt::SessionCancelParams>();
    r.add::<prompt::SessionCancelResult>();
    r.add::<prompt::QueueState>();
    r.add::<prompt::QueueEntry>();
    r.add::<prompt::QueueEntryKind>();
    r.add::<prompt::QueueRemoveParams>();
    r.add::<prompt::QueueEditParams>();
    r.add::<prompt::QueueMoveParams>();
    r.add::<prompt::QueueSessionParams>();
    r.add::<prompt::QueueListResult>();
    // request
    r.add::<request::Request>();
    r.add::<request::RequestKind>();
    r.add::<request::Choice>();
    r.add::<request::ChoiceIntent>();
    r.add::<request::ChoiceScope>();
    r.add::<request::Subject>();
    r.add::<request::Answer>();
    r.add::<request::Answerable>();
    r.add::<request::Notify>();
    r.add::<request::RequestStatus>();
    r.add::<request::RequestAnswerParams>();
    r.add::<request::RequestListParams>();
    r.add::<request::RequestListResult>();
    // option
    r.add::<option::ConfigOption>();
    r.add::<option::OptionRole>();
    r.add::<option::Applies>();
    r.add::<option::DependsOn>();
    r.add::<option::PermissionLevel>();
    r.add::<option::OptionSetParams>();
    r.add::<option::OptionSetResult>();
    r.add::<option::OptionCompleteParams>();
    r.add::<option::OptionCompleteResult>();
    r.add::<option::CompletionValue>();
    r.add::<option::OptionListParams>();
    r.add::<option::OptionListResult>();
    // action
    r.add::<action::Action>();
    r.add::<action::ActionScope>();
    r.add::<action::ActionWhen>();
    r.add::<action::Effect>();
    r.add::<action::Requirement>();
    r.add::<action::ActionSource>();
    r.add::<action::ActionInvokeParams>();
    r.add::<action::ActionInvokeResult>();
    r.add::<action::ActionStatus>();
    r.add::<action::ActionListParams>();
    r.add::<action::ActionListResult>();
    // usage
    r.add::<usage::Usage>();
    r.add::<usage::TokenUsage>();
    r.add::<usage::Cost>();
    r.add::<usage::ContextUsage>();
    r.add::<usage::RateLimit>();
    r.add::<usage::Budget>();
    r.add::<usage::BudgetState>();
    // driver
    r.add::<driver::BindingStart>();
    r.add::<driver::Pin>();
    r.add::<driver::McpInjection>();
    r.add::<driver::BindingStartResult>();
    r.add::<driver::BindingRef>();
    r.add::<driver::BindingPromptParams>();
    r.add::<driver::BindingPromptResult>();
    r.add::<driver::BindingDisposition>();
    r.add::<driver::BindingCancelParams>();
    r.add::<driver::BindingCancelResult>();
    r.add::<driver::BindingAnswerParams>();
    r.add::<driver::BindingOptionsParams>();
    r.add::<driver::BindingOptionsResult>();
    r.add::<driver::BindingActionParams>();
    r.add::<driver::BindingActionResult>();
    r.add::<driver::BindingForkParams>();
    r.add::<driver::BindingForkResult>();
    r.add::<driver::HistoryDiscoverResult>();
    r.add::<driver::HistoryReadParams>();
    r.add::<driver::HistoryReadResult>();
    r.add::<driver::BindingScreenResult>();
    r.add::<driver::ScreenPos>();
    r.add::<driver::ScreenSize>();
    r.add::<driver::BindingKeysParams>();
    r.add::<driver::DriverUpdate>();
    r.add::<driver::DriverUpdateKind>();
    // driver_data
    r.add::<driver_data::OptionsFile>();
    r.add::<driver_data::OptionData>();
    r.add::<driver_data::ChoiceData>();
    r.add::<driver_data::ChoiceFull>();
    r.add::<driver_data::DependsOnData>();
    r.add::<driver_data::WhenChoices>();
    r.add::<driver_data::LevelsFile>();
    r.add::<driver_data::LevelTools>();
    r.add::<driver_data::LevelMap>();
    r.add::<driver_data::PinArgs>();
    r.add::<driver_data::CorrelateFile>();
    r.add::<driver_data::CorrelateKind>();
    r.add::<driver_data::LimitsFile>();

    // The methods a client calls, from spec 39 and Appendix E8.
    r.method::<handshake::InitializeParams, handshake::InitializeResult>("initialize");
    r.method::<subscribe::SubscribeParams, subscribe::SubscribeResult>("subscribe");
    r.method::<subscribe::UnsubscribeParams, ids::Empty>("unsubscribe");
    r.method::<env::EnvListParams, env::EnvListResult>("env/list");
    r.method::<session::SessionNewParams, session::SessionNewResult>("session/new");
    r.method::<session::SessionListParams, session::SessionListResult>("session/list");
    r.method::<session::SessionGetParams, session::SessionGetResult>("session/get");
    r.method::<session::SessionStartParams, session::SessionStartResult>("session/start");
    r.method::<session::SessionStopParams, ids::Empty>("session/stop");
    r.method::<session::SessionRenameParams, ids::Empty>("session/rename");
    r.method::<session::SessionArchiveParams, ids::Empty>("session/archive");
    r.method::<session::SessionDeleteParams, ids::Empty>("session/delete");
    r.method::<session::SessionTreeParams, session::SessionTreeResult>("session/tree");
    r.method::<session::SessionRebindParams, session::SessionRebindResult>("session/rebind");
    r.method::<session::SessionRewindParams, ids::Empty>("session/rewind");
    r.method::<session::SessionRef, driver::BindingScreenResult>("session/screen");
    r.method::<session::SessionRef, ids::Empty>("session/keys");
    r.method::<prompt::SessionPromptParams, prompt::SessionPromptResult>("session/prompt");
    r.method::<prompt::SessionCancelParams, prompt::SessionCancelResult>("session/cancel");
    r.method::<prompt::QueueListParams, prompt::QueueListResult>("queue/list");
    r.method::<prompt::QueueClearParams, ids::Empty>("queue/clear");
    r.method::<prompt::QueueSessionParams, ids::Empty>("queue/pause");
    r.method::<prompt::QueueResumeParams, ids::Empty>("queue/resume");
    r.method::<prompt::QueueRemoveParams, ids::Empty>("queue/remove");
    r.method::<prompt::QueueEditParams, ids::Empty>("queue/edit");
    r.method::<prompt::QueueMoveParams, ids::Empty>("queue/move");
    r.method::<subscribe::ItemGetParams, subscribe::ItemGetResult>("item/get");
    r.method::<request::RequestListParams, request::RequestListResult>("request/list");
    r.method::<request::RequestAnswerParams, ids::Empty>("request/answer");
    r.method::<option::OptionListParams, option::OptionListResult>("option/list");
    r.method::<option::OptionSetParams, option::OptionSetResult>("option/set");
    r.method::<option::OptionCompleteParams, option::OptionCompleteResult>("option/complete");
    r.method::<action::ActionListParams, action::ActionListResult>("action/list");
    r.method::<action::ActionInvokeParams, action::ActionInvokeResult>("action/invoke");
    r.method::<agent::AgentListParams, agent::AgentListResult>("agent/list");
    r.method::<agent::AgentRefreshParams, agent::AgentRefreshResult>("agent/refresh");
    r.method::<workspace::WorkspaceListParams, workspace::WorkspaceListResult>("workspace/list");
    r.method::<workspace::WorkspaceCreateParams, workspace::WorkspaceCreateResult>("workspace/create");
    r.method::<workspace::WorkspaceStatusParams, workspace::WorkspaceStatusResult>("workspace/status");
    r.method::<workspace::WorkspaceMergeParams, workspace::WorkspaceMergeResult>("workspace/merge");
    r.method::<workspace::WorkspaceRemoveParams, ids::Empty>("workspace/remove");
    r.method::<workspace::WorkspaceTransferParams, workspace::WorkspaceTransferResult>("workspace/transfer");
    r.method::<session::SessionConfigureParams, ids::Empty>("session/configure");
    r.method::<seed::SessionBundleParams, seed::SessionBundleResult>("session/bundle");
    r.method::<subscribe::SessionReadParams, subscribe::SessionReadResult>("session/read");
    r.method::<session::SessionDiscoverParams, session::SessionDiscoverResult>("session/discover");
    r.method::<session::SessionAdoptParams, session::SessionAdoptResult>("session/adopt");
    r.method::<task::TaskAssignParams, task::TaskAssignResult>("task/assign");
    r.method::<task::TaskFinishParams, ids::Empty>("task/finish");
    r.method::<task::TaskFailParams, ids::Empty>("task/fail");
    r.method::<task::TaskWaitParams, task::TaskWaitResult>("task/wait");
    r.method::<function::FunctionListParams, function::FunctionListResult>("function/list");
    r.method::<function::FunctionSearchParams, function::FunctionListResult>("function/search");
    r.method::<function::FunctionDescribeParams, function::FunctionDescribeResult>("function/describe");
    r.method::<function::FunctionCallParams, function::FunctionResult>("function/call");
    r.method::<function::FunctionProvideParams, ids::Empty>("function/provide");
    r.method::<ids::Empty, principal::TokenListResult>("token/list");
    r.method::<principal::TokenCreateParams, principal::TokenCreateResult>("token/create");
    r.method::<principal::TokenRevokeParams, ids::Empty>("token/revoke");
    r.method::<ids::Empty, link::LinkListResult>("link/list");
    r.method::<ids::Empty, link::CarrierListResult>("carrier/list");
    r.method::<link::LinkInviteParams, link::LinkInviteResult>("link/invite");
    r.method::<link::LinkAddParams, link::LinkResult>("link/add");
    r.method::<link::LinkUpdateParams, link::LinkResult>("link/update");
    r.method::<link::LinkRemoveParams, ids::Empty>("link/remove");
    r.method::<link::BlobPutParams, ids::Empty>("blob/put");
    r.method::<link::SessionOfferParams, ids::Empty>("session/offer");
    r.method::<link::SessionCommitParams, ids::Empty>("session/commit");
    // The requests a runtime sends to a client: calls of functions it provides (spec 25.2).
    r.client_method::<function::FunctionCallParams, function::FunctionResult>("function/call");

    // The methods a runtime calls on a driver (spec 30.1).
    r.method::<driver::BindingStartParams, driver::BindingStartResult>("binding/start");
    r.method::<driver::BindingStopParams, ids::Empty>("binding/stop");
    r.method::<driver::BindingPromptParams, driver::BindingPromptResult>("binding/prompt");
    r.method::<driver::BindingCancelParams, driver::BindingCancelResult>("binding/cancel");
    r.method::<driver::BindingAnswerParams, ids::Empty>("binding/answer");
    r.method::<driver::BindingOptionsParams, driver::BindingOptionsResult>("binding/options");
    r.method::<driver::BindingActionParams, driver::BindingActionResult>("binding/action");
    r.method::<driver::BindingForkParams, driver::BindingForkResult>("binding/fork");
    r.method::<driver::BindingScreenParams, driver::BindingScreenResult>("binding/screen");
    r.method::<driver::BindingKeysParams, ids::Empty>("binding/keys");
    r.method::<driver::HistoryDiscoverParams, driver::HistoryDiscoverResult>("history/discover");
    r.method::<driver::HistoryReadParams, driver::HistoryReadResult>("history/read");

    // The notifications, by the params each carries.
    r.notification::<subscribe::UpdateParams>("update");
}

/// Collects types and methods into one document.
pub struct Registry {
    generator: SchemaGenerator,
    roots: Map<String, Value>,
    methods: BTreeMap<String, Value>,
    client_methods: BTreeMap<String, Value>,
    notifications: BTreeMap<String, Value>,
}

impl Registry {
    pub fn add<T: JsonSchema>(&mut self) {
        let s = serde_json::to_value(self.generator.subschema_for::<T>()).expect("serialize");
        // Types schemars inlines rather than references are stored under their own name.
        if s.get("$ref").is_none() {
            self.roots.insert(T::schema_name().into_owned(), s);
        }
    }

    /// A method's params and result, each as a `$ref` into `$defs`.
    pub fn method<P: JsonSchema, R: JsonSchema>(&mut self, name: &str) {
        self.add::<P>();
        self.add::<R>();
        self.methods.insert(
            name.to_string(),
            json!({ "params": ref_to::<P>(), "result": ref_to::<R>() }),
        );
    }

    /// A request the runtime sends to a client, with its params and result.
    pub fn client_method<P: JsonSchema, R: JsonSchema>(&mut self, name: &str) {
        self.add::<P>();
        self.add::<R>();
        self.client_methods.insert(
            name.to_string(),
            json!({ "params": ref_to::<P>(), "result": ref_to::<R>() }),
        );
    }

    /// A notification's params, as a `$ref` into `$defs`.
    pub fn notification<P: JsonSchema>(&mut self, name: &str) {
        self.add::<P>();
        self.notifications.insert(name.to_string(), ref_to::<P>());
    }
}

/// One document holding every registered type under `$defs` and the method table alongside it.
pub fn all() -> Value {
    let mut r = Registry {
        generator: SchemaGenerator::default(),
        roots: Map::new(),
        methods: BTreeMap::new(),
        client_methods: BTreeMap::new(),
        notifications: BTreeMap::new(),
    };
    register(&mut r);
    let mut defs: Map<String, Value> = r.generator.definitions().clone();
    defs.extend(r.roots);
    defs.sort_keys();
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Parrot protocol, version 1",
        "$defs": defs,
        "x-methods": r.methods,
        "x-client-methods": r.client_methods,
        "x-notifications": r.notifications,
    })
}

/// Schema-only copies of the frozen `rpc` envelopes, which don't derive `JsonSchema`.
/// They must keep the same serde shape as `crate::rpc`.
mod envelope {
    use schemars::JsonSchema;
    use serde_json::Value;

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    #[schemars(rename = "EnvelopeRequest")]
    pub struct EnvelopeRequest {
        pub jsonrpc: String,
        pub id: Value,
        pub method: String,
        #[serde(default)]
        pub params: Option<Value>,
    }

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    #[schemars(rename = "EnvelopeNotification")]
    pub struct EnvelopeNotification {
        pub jsonrpc: String,
        pub method: String,
        #[serde(default)]
        pub params: Option<Value>,
    }

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    #[schemars(rename = "EnvelopeResponse")]
    pub struct EnvelopeResponse {
        pub jsonrpc: String,
        pub id: Value,
        #[serde(default)]
        pub result: Option<Value>,
        #[serde(default)]
        pub error: Option<RpcError>,
    }

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    pub struct RpcError {
        pub code: i64,
        pub message: String,
        #[serde(default)]
        pub data: Option<Value>,
    }

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    #[serde(untagged)]
    pub enum Message {
        Request(EnvelopeRequest),
        Response(EnvelopeResponse),
        Notification(EnvelopeNotification),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// Fails when the committed schema is stale; `PARROT_BLESS=1` rewrites it.
    #[test]
    fn committed_schema_is_current() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec/schema/parrot.schema.json");
        let fresh = serde_json::to_string_pretty(&super::all()).unwrap() + "\n";
        if std::env::var_os("PARROT_BLESS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        if committed != fresh {
            let diff: Vec<String> = committed
                .lines()
                .zip(fresh.lines())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .take(20)
                .map(|(i, (a, b))| format!("line {}:\n- {a}\n+ {b}", i + 1))
                .collect();
            panic!(
                "spec/schema/parrot.schema.json is stale ({} vs {} lines); run with PARROT_BLESS=1\n{}",
                committed.lines().count(),
                fresh.lines().count(),
                diff.join("\n")
            );
        }
    }

    #[test]
    fn has_message() {
        let all = super::all();
        assert!(all["$defs"]["Message"].is_object());
        assert!(all["$defs"]["MovedData"].is_object());
        assert!(super::schema_for::<super::error::MovedData>()["properties"]["runtime"].is_object());
    }

    /// Every method and notification in the table points at a definition that exists.
    #[test]
    fn methods_resolve_to_defs() {
        let all = super::all();
        let defs = &all["$defs"];
        let mut names = 0;
        let client = all["x-client-methods"].as_object().unwrap().iter();
        for (name, method) in all["x-methods"].as_object().unwrap().iter().chain(client) {
            for key in ["params", "result"] {
                let r = method[key]["$ref"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{name}.{key} is not a $ref"));
                let def = r.trim_start_matches("#/$defs/");
                assert!(defs.get(def).is_some(), "{name}.{key} points at missing {def}");
            }
            names += 1;
        }
        assert!(names > 40, "only {names} methods registered");
        for (name, params) in all["x-notifications"].as_object().unwrap() {
            let def = params["$ref"].as_str().unwrap().trim_start_matches("#/$defs/");
            assert!(defs.get(def).is_some(), "{name} points at missing {def}");
        }
    }
}
