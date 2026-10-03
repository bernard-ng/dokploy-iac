//! `dokploy-sim`: an in-memory Dokploy for tests (ADR 0015).
//!
//! The simulator is reached through [`Transport`], so the engine runs against it in-process
//! with no socket. It contains no knowledge of any kind: it stores one object per create,
//! per kind, and answers exactly the operations the loaded kind specs reference.
//!
//! - **Contract.** Every request is checked against the generated request contract of its
//!   operation: an unknown or missing field is a 400, as at the real Dokploy.
//! - **Behavior** comes from the spec: `create` stores an object (and refuses a missing
//!   parent), `update` merges the fields it carries (patch), `remove` deletes it and what
//!   hangs below it, `list` returns the collection (filtered by any query parameter that
//!   names a field), `one` returns one object with the child collections the specs say are
//!   embedded in it.
//! - **Response shapes** are copied from recorded fixtures when a fixture directory is given
//!   ([`Sim::with_fixtures`]): which keys a response has, whether an update answers `true`.
//! - **Faults** ([`Fault`]) drop the connection before or after the request, reject it, or
//!   save and then fail.
//!
//! It is a development dependency, never shipped, and it never replaces live acceptance: the
//! same scenarios run against a real Dokploy (ADR 0015).

mod fault;
mod shape;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use dokploy_api::{BodyShape, EndpointMethod, endpoint_by_operation, request_contract};
use dokploy_sdk::{DokployError, Error, OperationRequest, Transport};
use dokploy_spec::{CreateIdentity, KindSpec, SpecRegistry, WriteGroup};
use serde_json::{Map, Value};
use url::Url;

pub use fault::{Fault, FaultKind};
use shape::{Role, Shapes, fill, fill_list};

type Object = Map<String, Value>;

const CREATED_AT: &str = "2026-01-01T00:00:00.000Z";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Create,
    Update,
    Remove,
    List,
    One,
}

#[derive(Clone, Debug)]
struct Route {
    kind: String,
    action: Action,
}

/// One request the simulator received.
#[derive(Clone)]
pub struct Logged {
    operation: String,
    query: BTreeMap<String, Value>,
    body: Option<Value>,
    mutation: bool,
}

impl Logged {
    /// The operation id.
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// The query parameters.
    #[must_use]
    pub fn query(&self) -> &BTreeMap<String, Value> {
        &self.query
    }

    /// The JSON body, which can hold a secret: tests only.
    #[must_use]
    pub fn body(&self) -> Option<&Value> {
        self.body.as_ref()
    }

    /// Whether the operation changes Dokploy.
    #[must_use]
    pub fn is_mutation(&self) -> bool {
        self.mutation
    }
}

impl std::fmt::Debug for Logged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Logged")
            .field("operation", &self.operation)
            .field("query_names", &self.query.keys().collect::<Vec<_>>())
            .field("has_body", &self.body.is_some())
            .finish()
    }
}

type Tamper = Arc<dyn Fn(&mut Value) + Send + Sync>;

#[derive(Default)]
struct Inner {
    tampers: BTreeMap<String, Tamper>,
    objects: BTreeMap<String, Vec<Object>>,
    next_id: u64,
    calls: BTreeMap<String, usize>,
    faults: Vec<Fault>,
    log: Vec<Logged>,
    hidden: BTreeSet<(String, String)>,
}

/// An in-memory Dokploy.
pub struct Sim {
    specs: Arc<SpecRegistry>,
    routes: BTreeMap<String, Vec<Route>>,
    base_url: Url,
    shapes: Shapes,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for Sim {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock();
        formatter
            .debug_struct("Sim")
            .field("kinds", &inner.objects.keys().collect::<Vec<_>>())
            .field("requests", &inner.log.len())
            .finish()
    }
}

impl Sim {
    /// A simulator that answers the operations `specs` reference.
    #[must_use]
    pub fn new(specs: Arc<SpecRegistry>) -> Self {
        let mut routes: BTreeMap<String, Vec<Route>> = BTreeMap::new();
        let mut add = |operation: &str, kind: &str, action: Action| {
            routes.entry(operation.to_owned()).or_default().push(Route {
                kind: kind.to_owned(),
                action,
            });
        };
        for spec in specs.kinds() {
            let kind = spec.kind.as_str();
            if let Some(op) = &spec.api.create {
                add(&op.op, kind, Action::Create);
            }
            if let Some(op) = &spec.api.update {
                add(&op.op, kind, Action::Update);
            }
            for group in &spec.write {
                match group {
                    WriteGroup::Op { op, .. } => add(op, kind, Action::Update),
                    WriteGroup::ByVariant { ops, .. } => {
                        for op in ops.values() {
                            add(op, kind, Action::Update);
                        }
                    }
                }
            }
            if let Some(op) = &spec.api.remove {
                add(&op.op, kind, Action::Remove);
            }
            if let Some(op) = spec
                .api
                .read
                .list
                .as_ref()
                .and_then(|list| list.op.as_ref())
            {
                add(op, kind, Action::List);
            }
            if let Some(one) = &spec.api.read.one {
                add(&one.op, kind, Action::One);
            }
        }
        for entries in routes.values_mut() {
            entries.dedup_by(|a, b| a.kind == b.kind && a.action == b.action);
        }

        Self {
            specs,
            routes,
            base_url: Url::parse("https://sim.dokploy.test/").expect("a constant URL"),
            shapes: Shapes::default(),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Takes response shapes from the recorded fixtures in `directory`
    /// (`fixtures/api/live/<version>`).
    #[must_use]
    pub fn with_fixtures(mut self, directory: &Path) -> Self {
        self.shapes = Shapes::from_directory(directory);
        self
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Queues a fault.
    pub fn inject(&self, fault: Fault) {
        self.lock().faults.push(fault);
    }

    /// Rewrites every successful response of `operation` with `rewrite`, until it is replaced:
    /// a Dokploy that says something other than what is stored.
    pub fn tamper(&self, operation: &str, rewrite: impl Fn(&mut Value) + Send + Sync + 'static) {
        self.lock()
            .tampers
            .insert(operation.to_owned(), Arc::new(rewrite));
    }

    /// Every request received so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<Logged> {
        self.lock().log.clone()
    }

    /// The requests that changed Dokploy (or tried to), in order.
    #[must_use]
    pub fn mutations(&self) -> Vec<Logged> {
        self.lock()
            .log
            .iter()
            .filter(|l| l.mutation)
            .cloned()
            .collect()
    }

    /// Forgets the request log (the objects stay).
    pub fn clear_requests(&self) {
        self.lock().log.clear();
    }

    /// Adds an object as if it had always been there, without a request. Returns its id.
    /// Missing nullable fields are `null`, the id and server-owned fields are filled in; an id
    /// in `fields` is kept.
    ///
    /// # Panics
    ///
    /// When `kind` has no spec or `fields` is not an object.
    pub fn seed(&self, kind: &str, fields: &Value) -> String {
        let spec = self.specs.get(kind).expect("a kind with a spec");
        let fields = fields.as_object().expect("an object of fields");
        let mut inner = self.lock();
        let object = new_object(spec, fields, &mut inner);
        let id = id_of(spec, &object).expect("a new object has an id");
        inner
            .objects
            .entry(kind.to_owned())
            .or_default()
            .push(object);

        id
    }

    /// Every object of a kind, as stored (secrets included).
    #[must_use]
    pub fn objects(&self, kind: &str) -> Vec<Value> {
        self.lock()
            .objects
            .get(kind)
            .map(|all| all.iter().cloned().map(Value::Object).collect())
            .unwrap_or_default()
    }

    /// One object by id, as stored.
    #[must_use]
    pub fn object(&self, kind: &str, id: &str) -> Option<Value> {
        let spec = self.specs.get(kind)?;
        self.lock()
            .objects
            .get(kind)?
            .iter()
            .find(|object| id_of(spec, object).as_deref() == Some(id))
            .cloned()
            .map(Value::Object)
    }

    /// Changes fields of an object behind the engine's back (drift).
    ///
    /// # Panics
    ///
    /// When the object does not exist.
    pub fn patch(&self, kind: &str, id: &str, fields: &Value) {
        let spec = self.specs.get(kind).expect("a kind with a spec");
        let mut inner = self.lock();
        let object = inner
            .objects
            .get_mut(kind)
            .and_then(|all| {
                all.iter_mut()
                    .find(|o| id_of(spec, o).as_deref() == Some(id))
            })
            .expect("an existing object");
        for (key, value) in fields.as_object().expect("an object of fields") {
            object.insert(key.clone(), value.clone());
        }
    }

    /// Deletes an object, and what hangs below it, behind the engine's back.
    pub fn delete(&self, kind: &str, id: &str) {
        let mut inner = self.lock();
        cascade_remove(&self.specs, &mut inner, kind, id);
    }

    /// Hides an object from collection reads while a direct read still finds it, as a role
    /// filter would.
    pub fn hide_from_listing(&self, kind: &str, id: &str) {
        self.lock().hidden.insert((kind.to_owned(), id.to_owned()));
    }

    fn handle(&self, request: &OperationRequest) -> Result<Value, Error> {
        let operation = request.operation();
        let Some(endpoint) = endpoint_by_operation(operation) else {
            return Err(Error::InvalidRequest {
                operation: "unknown",
                source: anyhow::anyhow!("operation is not declared by the pinned Dokploy contract"),
            });
        };
        let wire = endpoint.operation();
        let mutation = endpoint.method() == EndpointMethod::Post;

        let mut inner = self.lock();
        inner.log.push(Logged {
            operation: operation.to_owned(),
            query: request.query_parameters().clone(),
            body: request.json_body().cloned(),
            mutation,
        });
        let ordinal = {
            let count = inner.calls.entry(operation.to_owned()).or_default();
            *count += 1;
            *count
        };
        let fault = take_fault(&mut inner.faults, operation, ordinal);

        match fault {
            Some(FaultKind::DropBefore) => return Err(dropped(wire, mutation)),
            Some(FaultKind::Reject { status }) => return Err(rejected(status)),
            Some(FaultKind::Unavailable) => return Err(rejected(503)),
            Some(
                FaultKind::DropAfter
                | FaultKind::RejectAfter { .. }
                | FaultKind::Swallow
                | FaultKind::Duplicate,
            )
            | None => {}
        }

        let mut outcome = if fault == Some(FaultKind::Swallow) {
            Ok(Value::Bool(true))
        } else {
            self.execute(&mut inner, request)
        };
        if fault == Some(FaultKind::Duplicate) && outcome.is_ok() {
            self.duplicate_last(&mut inner, request);
        }
        if let (Ok(response), Some(rewrite)) = (&mut outcome, inner.tampers.get(operation).cloned())
        {
            rewrite(response);
        }

        match (fault, outcome) {
            (Some(FaultKind::DropAfter), Ok(_)) => Err(dropped(wire, mutation)),
            (Some(FaultKind::RejectAfter { status }), Ok(_)) => Err(rejected(status)),
            (_, Ok(value)) => Ok(value),
            (_, Err(error)) => Err(Error::Api(error)),
        }
    }

    /// Adds a copy of the object the create just made, under a new identity.
    fn duplicate_last(&self, inner: &mut Inner, request: &OperationRequest) {
        let Some(route) = self
            .routes
            .get(request.operation())
            .and_then(|routes| routes.iter().find(|r| r.action == Action::Create))
        else {
            return;
        };
        let spec = self
            .specs
            .get(&route.kind)
            .expect("a routed kind has a spec");
        let Some(mut copy) = inner
            .objects
            .get(&route.kind)
            .and_then(|all| all.last())
            .cloned()
        else {
            return;
        };
        inner.next_id += 1;
        copy.insert(
            spec.api.id.clone(),
            Value::String(format!("sim-{}-{}", spec.kind, inner.next_id)),
        );
        inner
            .objects
            .entry(route.kind.clone())
            .or_default()
            .push(copy);
    }

    fn execute(
        &self,
        inner: &mut Inner,
        request: &OperationRequest,
    ) -> Result<Value, DokployError> {
        let operation = request.operation();
        check_contract(operation, request)?;
        let Some(routes) = self.routes.get(operation) else {
            return Err(api_error(
                404,
                "NOT_FOUND",
                format!("no route for {operation}"),
            ));
        };

        let mut last = Err(api_error(
            404,
            "NOT_FOUND",
            format!("no route for {operation}"),
        ));
        for route in routes {
            let spec = self
                .specs
                .get(&route.kind)
                .expect("a routed kind has a spec");
            last = match route.action {
                Action::Create => self.create(inner, spec, request),
                Action::Update => self.update(inner, spec, request),
                Action::Remove => self.remove(inner, spec, request),
                Action::List => Ok(self.list(inner, spec, request)),
                Action::One => self.one(inner, spec, request),
            };
            if last.is_ok() {
                break;
            }
        }

        last
    }

    fn create(
        &self,
        inner: &mut Inner,
        spec: &KindSpec,
        request: &OperationRequest,
    ) -> Result<Value, DokployError> {
        let body = body_object(request)?;
        if let Some(first) = spec.parents.first() {
            // The parent is whichever of the possible parent kinds holds the id the body names.
            let create = spec.api.create.as_ref();
            let held = spec.parents.iter().any(|parent| {
                let Some(parent_spec) = self.specs.get(parent) else {
                    return false;
                };
                create
                    .and_then(|op| op.parent_id_field(Some(parent)))
                    .and_then(|field| body.get(field))
                    .and_then(Value::as_str)
                    .is_some_and(|id| find(inner, parent_spec, id).is_some())
            });
            if !held {
                let parent_spec = self.specs.get(first).expect("a parent kind has a spec");
                return Err(not_found(parent_spec));
            }
        }
        let mut object = new_object(spec, &body, inner);
        // The row names its parent in a column of its own, whatever the request called it.
        for parent in &spec.parents {
            let id = spec
                .api
                .create
                .as_ref()
                .and_then(|op| op.parent_id_field(Some(parent)))
                .and_then(|field| body.get(field))
                .and_then(Value::as_str);
            if let (Some(column), Some(id)) = (spec.parent_column(parent), id) {
                let parent_spec = self.specs.get(parent).expect("a parent kind has a spec");
                if find(inner, parent_spec, id).is_some() {
                    object.insert(column.to_owned(), Value::String(id.to_owned()));
                }
            }
        }
        // A column the request did not set holds the value a fresh row held in the capture.
        if let Some(Value::Object(captured)) = self
            .shapes
            .template(request.operation(), Role::Mutation)
            .filter(|template| template.get("success").is_none())
        {
            for (key, value) in captured {
                if matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
                    object.entry(key).or_insert(value);
                }
            }
        }
        // The columns of every arm of a union exist, and hold nothing until the arm is saved.
        for field in spec.fields.values() {
            for members in field.arms.values() {
                for (member, member_field) in members {
                    object
                        .entry(member_field.request_name(member).to_owned())
                        .or_insert(Value::Null);
                }
            }
        }
        let stored = Value::Object(object.clone());
        inner
            .objects
            .entry(spec.kind.clone())
            .or_default()
            .push(object.clone());

        let operation = request.operation();
        if let Some(template) = self.shapes.template(operation, Role::Mutation) {
            return Ok(fill(&template, &object));
        }

        Ok(match &spec.api.create_identity {
            Some(CreateIdentity::FromResponse(pointer)) => nest(pointer, stored),
            _ => Value::Bool(true),
        })
    }

    fn update(
        &self,
        inner: &mut Inner,
        spec: &KindSpec,
        request: &OperationRequest,
    ) -> Result<Value, DokployError> {
        let body = body_object(request)?;
        let id = body
            .get(&spec.api.id)
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "BAD_REQUEST", format!("{} is required", spec.api.id)))?
            .to_owned();
        let object = inner
            .objects
            .get_mut(&spec.kind)
            .and_then(|all| {
                all.iter_mut()
                    .find(|o| id_of(spec, o).as_deref() == Some(&id))
            })
            .ok_or_else(|| not_found(spec))?;
        for (key, value) in &body {
            if *key != spec.api.id {
                object.insert(key.clone(), value.clone());
            }
        }
        let object = object.clone();

        Ok(
            match self.shapes.template(request.operation(), Role::Mutation) {
                Some(template) => fill(&template, &object),
                None => Value::Bool(true),
            },
        )
    }

    fn remove(
        &self,
        inner: &mut Inner,
        spec: &KindSpec,
        request: &OperationRequest,
    ) -> Result<Value, DokployError> {
        let body = body_object(request)?;
        let id = body
            .get(&spec.api.id)
            .and_then(Value::as_str)
            .ok_or_else(|| api_error(400, "BAD_REQUEST", format!("{} is required", spec.api.id)))?
            .to_owned();
        let removed = find(inner, spec, &id)
            .cloned()
            .ok_or_else(|| not_found(spec))?;
        cascade_remove(&self.specs, inner, &spec.kind, &id);

        Ok(
            match self.shapes.template(request.operation(), Role::Mutation) {
                Some(template) => fill(&template, &removed),
                None => Value::Bool(true),
            },
        )
    }

    fn list(&self, inner: &Inner, spec: &KindSpec, request: &OperationRequest) -> Value {
        let items: Vec<&Object> = inner
            .objects
            .get(&spec.kind)
            .into_iter()
            .flatten()
            .filter(|object| {
                request
                    .query_parameters()
                    .iter()
                    .all(|(name, value)| object.get(name) == Some(value))
            })
            .filter(|object| {
                id_of(spec, object)
                    .is_none_or(|id| !inner.hidden.contains(&(spec.kind.clone(), id)))
            })
            .collect();

        match self.shapes.template(request.operation(), Role::List) {
            Some(template) => fill_list(&template, &items),
            None => Value::Array(
                items
                    .into_iter()
                    .map(|o| Value::Object(o.clone()))
                    .collect(),
            ),
        }
    }

    fn one(
        &self,
        inner: &Inner,
        spec: &KindSpec,
        request: &OperationRequest,
    ) -> Result<Value, DokployError> {
        let one = spec.api.read.one.as_ref().expect("a routed read has an op");
        let id = request
            .query_parameters()
            .get(&one.id_param)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                api_error(400, "BAD_REQUEST", format!("{} is required", one.id_param))
            })?;
        let object = find(inner, spec, id).ok_or_else(|| not_found(spec))?;
        let mut value = match self.shapes.template(request.operation(), Role::One) {
            Some(template) => fill(&template, object),
            None => Value::Object(object.clone()),
        };
        for child in self.specs.children_of(&spec.kind) {
            let Some(embedded) = child
                .api
                .read
                .list
                .as_ref()
                .and_then(|l| l.embedded_in.as_ref())
            else {
                continue;
            };
            let parent_op = embedded
                .parent_op
                .as_deref()
                .or_else(|| spec.api.read.one.as_ref().map(|one| one.op.as_str()));
            if parent_op != Some(request.operation()) {
                continue;
            }
            let field = child.parent_column(&spec.kind).map(str::to_owned);
            let Some(field) = field else { continue };
            let children: Vec<Value> = inner
                .objects
                .get(&child.kind)
                .into_iter()
                .flatten()
                .filter(|o| o.get(&field).and_then(Value::as_str) == Some(id))
                .cloned()
                .map(Value::Object)
                .collect();
            set_pointer(&mut value, &embedded.pointer, Value::Array(children));
        }

        Ok(value)
    }
}

impl Transport for Sim {
    fn base_url(&self) -> &Url {
        &self.base_url
    }

    async fn call(&self, request: OperationRequest) -> Result<Value, Error> {
        self.handle(&request)
    }
}

fn take_fault(faults: &mut Vec<Fault>, operation: &str, ordinal: usize) -> Option<FaultKind> {
    let position = faults
        .iter()
        .position(|f| f.operation == operation && f.call.is_none_or(|n| n == ordinal))?;

    Some(faults.remove(position).kind)
}

fn dropped(wire: &'static str, mutation: bool) -> Error {
    let source = anyhow::anyhow!("the connection was dropped");
    if mutation {
        Error::OutcomeUnknown {
            operation: wire,
            source,
        }
    } else {
        Error::Request {
            operation: wire,
            source,
        }
    }
}

fn rejected(status: u16) -> Error {
    let (code, message) = match status {
        400 => ("BAD_REQUEST", "the request was rejected"),
        401 => ("UNAUTHORIZED", "unauthorized"),
        403 => ("FORBIDDEN", "forbidden"),
        404 => ("NOT_FOUND", "not found"),
        _ => ("INTERNAL_SERVER_ERROR", "internal server error"),
    };

    Error::Api(api_error(status, code, message.to_owned()))
}

fn api_error(status: u16, code: &str, message: String) -> DokployError {
    DokployError::new(status, code.to_owned(), message, Vec::new())
}

fn not_found(spec: &KindSpec) -> DokployError {
    let mut name = spec.kind.clone();
    if let Some(first) = name.get_mut(0..1) {
        first.make_ascii_uppercase();
    }

    api_error(404, "NOT_FOUND", format!("{name} not found"))
}

fn body_object(request: &OperationRequest) -> Result<Object, DokployError> {
    match request.json_body() {
        Some(Value::Object(object)) => Ok(object.clone()),
        _ => Err(api_error(
            400,
            "BAD_REQUEST",
            "a JSON object body is required".to_owned(),
        )),
    }
}

/// Refuses what the real Dokploy refuses by schema: an unknown or missing field.
fn check_contract(operation: &str, request: &OperationRequest) -> Result<(), DokployError> {
    let Some(contract) = request_contract(operation) else {
        return Ok(());
    };
    let mut issues = Vec::new();
    for field in contract.query() {
        if field.required() && !request.query_parameters().contains_key(field.name()) {
            issues.push(format!("query `{}` is required", field.name()));
        }
    }
    for name in request.query_parameters().keys() {
        if !contract.query().iter().any(|field| field.name() == name) {
            issues.push(format!("query `{name}` is not accepted"));
        }
    }
    match (contract.body(), request.json_body()) {
        (BodyShape::Object(fields), Some(Value::Object(body))) => {
            for field in fields.iter().filter(|field| field.required()) {
                if !body.contains_key(field.name()) {
                    issues.push(format!("`{}` is required", field.name()));
                }
            }
            for name in body.keys() {
                if !fields.iter().any(|field| field.name() == name) {
                    issues.push(format!("`{name}` is not accepted"));
                }
            }
        }
        (BodyShape::Object(fields), None) if fields.iter().any(|f| f.required()) => {
            issues.push("a JSON body is required".to_owned());
        }
        (BodyShape::None, Some(_)) => issues.push("this operation takes no body".to_owned()),
        _ => {}
    }
    if issues.is_empty() {
        return Ok(());
    }

    Err(DokployError::new(
        400,
        "BAD_REQUEST".to_owned(),
        "the request does not match the contract".to_owned(),
        issues,
    ))
}

fn new_object(spec: &KindSpec, fields: &Object, inner: &mut Inner) -> Object {
    let mut object = Object::new();
    // A nullable column that was not given is null; the simulator cannot invent a default for
    // a column that must have a value, so it leaves it unreturned rather than wrong.
    for (name, field) in &spec.fields {
        // An environment block is empty as `null`, and the members of a struct are columns of
        // their own.
        let is_environment = dokploy_spec::parse_type(&field.ty)
            .is_ok_and(|ty| matches!(ty, dokploy_spec::FieldType::Env));
        if field.nullable || is_environment {
            object.insert(field.request_name(name).to_owned(), Value::Null);
        }
        for (member, member_field) in &field.members {
            if member_field.nullable {
                object.insert(member_field.request_name(member).to_owned(), Value::Null);
            }
        }
    }
    for name in &spec.ledger.readonly {
        let value = if name == "createdAt" {
            CREATED_AT.to_owned()
        } else {
            format!("sim-{name}")
        };
        object.insert(name.clone(), Value::String(value));
    }
    for (key, value) in fields {
        object.insert(key.clone(), value.clone());
    }
    inner.next_id += 1;
    // A seeded object may bring its own id, so a test can name it in advance.
    if !fields.contains_key(&spec.api.id) {
        object.insert(
            spec.api.id.clone(),
            Value::String(format!("sim-{}-{}", spec.kind, inner.next_id)),
        );
    }

    object
}

fn id_of(spec: &KindSpec, object: &Object) -> Option<String> {
    object.get(&spec.api.id)?.as_str().map(str::to_owned)
}

fn find<'a>(inner: &'a Inner, spec: &KindSpec, id: &str) -> Option<&'a Object> {
    inner
        .objects
        .get(&spec.kind)?
        .iter()
        .find(|object| id_of(spec, object).as_deref() == Some(id))
}

/// Removes an object and everything attached below it, as the database's cascade would.
fn cascade_remove(specs: &SpecRegistry, inner: &mut Inner, kind: &str, id: &str) {
    let Some(spec) = specs.get(kind) else { return };
    if let Some(all) = inner.objects.get_mut(kind) {
        all.retain(|object| id_of(spec, object).as_deref() != Some(id));
    }
    for child in specs.children_of(kind) {
        let field = child.parent_column(kind).map(str::to_owned);
        let Some(field) = field else { continue };
        let doomed: Vec<String> = inner
            .objects
            .get(&child.kind)
            .into_iter()
            .flatten()
            .filter(|o| o.get(&field).and_then(Value::as_str) == Some(id))
            .filter_map(|o| id_of(child, o))
            .collect();
        for child_id in doomed {
            cascade_remove(specs, inner, &child.kind, &child_id);
        }
    }
}

/// Wraps `object` so that the id lives at `pointer`: `/id` is the object itself,
/// `/project/projectId` is `{"project": object}`.
fn nest(pointer: &str, object: Value) -> Value {
    let segments: Vec<&str> = pointer.trim_start_matches('/').split('/').collect();
    segments[..segments.len().saturating_sub(1)]
        .iter()
        .rev()
        .fold(object, |inner, key| {
            let mut wrapper = Object::new();
            wrapper.insert((*key).to_owned(), inner);
            Value::Object(wrapper)
        })
}

fn set_pointer(value: &mut Value, pointer: &str, replacement: Value) {
    let mut segments = pointer.trim_start_matches('/').split('/').peekable();
    let mut current = value;
    while let Some(segment) = segments.next() {
        let Value::Object(map) = current else { return };
        if segments.peek().is_none() {
            map.insert(segment.to_owned(), replacement);
            return;
        }
        current = map
            .entry(segment.to_owned())
            .or_insert_with(|| Value::Object(Object::new()));
    }
}
