//! The MongoDB executor: [`MongoBackend`] and its transactions.

use super::compile::{
    CompiledDelete, CompiledInsert, CompiledQuery, CompiledUpdate, CompiledWrite, Keys, UpdateSpec,
    compile_query, compile_write,
};
use super::value::from_bson;
use ::mongodb::action::Action;
use ::mongodb::bson::{Bson, Document, doc};
use ::mongodb::error::{Error as DriverError, ErrorKind};
use ::mongodb::options::ReturnDocument;
use ::mongodb::{Client, ClientSession, Collection, Cursor, Database};
use async_trait::async_trait;
use siderite_orm::{
    Backend, BackendCapabilities, BackendCapabilityError, BackendError, BackendKind, ExecResult,
    Executor, Feature, IsolationLevel, OrmError, QueryError, QueryPlan, QueryResult, Row,
    Transaction, Value, WritePlan,
};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Collection holding one `{_id: <collection>, seq: <last key>}` document per
/// collection with generated integer keys.
const COUNTERS: &str = "siderite_counters";

/// MongoDB adapter: compiles plans to filters and pipelines and runs them.
///
/// Collections are named after tables. The primary-key column of a
/// collection is stored as `_id` (see [`Keys`]); rows come back with `_id`
/// renamed to that column. Integer keys that a plan does not supply are
/// generated from a counter collection, `siderite_counters`.
#[derive(Debug, Clone)]
pub struct MongoBackend {
    client: Client,
    db: Database,
    keys: Arc<Keys>,
}

impl MongoBackend {
    /// Connect to `url` and use database `database`.
    ///
    /// # Errors
    /// [`BackendError::Connection`] if the URL is invalid or no server can be
    /// reached.
    pub async fn connect(url: &str, database: &str) -> Result<Self, BackendError> {
        let client = Client::with_uri_str(url)
            .await
            .map_err(|e| BackendError::Connection(e.to_string()))?;
        client
            .database("admin")
            .run_command(doc! { "ping": 1_i32 })
            .await
            .map_err(|e| BackendError::Connection(e.to_string()))?;
        Ok(Self::from_client(client, database))
    }

    /// Use an existing client.
    pub fn from_client(client: Client, database: &str) -> Self {
        Self {
            db: client.database(database),
            client,
            keys: Arc::new(Keys::default()),
        }
    }

    /// Set which column of each collection is stored as `_id` (default: a
    /// column named `id`).
    #[must_use]
    pub fn with_keys(mut self, keys: Keys) -> Self {
        self.keys = Arc::new(keys);
        self
    }

    /// The database this adapter works in.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// Run a database command (`db.runCommand`), the escape hatch for what
    /// plans cannot express.
    ///
    /// # Errors
    /// [`BackendError`] when the server rejects the command.
    pub async fn raw_command(&self, command: Document) -> Result<Document, OrmError> {
        self.db
            .run_command(command)
            .await
            .map_err(|e| map_error(&e).into())
    }

    /// Create a unique index on `column` of `collection`, so duplicate
    /// values fail with [`BackendError::Constraint`] like a SQL unique
    /// constraint. MongoDB enforces uniqueness only for `_id` otherwise.
    ///
    /// # Errors
    /// [`BackendError`] when the index cannot be created.
    pub async fn create_unique_index(
        &self,
        collection: &str,
        column: &str,
    ) -> Result<(), OrmError> {
        let field = if column == self.keys.pk(collection) {
            "_id"
        } else {
            column
        };
        let index = ::mongodb::IndexModel::builder()
            .keys(doc! { field: 1_i32 })
            .options(
                ::mongodb::options::IndexOptions::builder()
                    .unique(true)
                    .build(),
            )
            .build();
        self.db
            .collection::<Document>(collection)
            .create_index(index)
            .await
            .map_err(|e| OrmError::from(map_error(&e)))?;
        Ok(())
    }

    async fn run_fetch(
        &self,
        plan: &QueryPlan,
        session: Option<&mut ClientSession>,
    ) -> Result<QueryResult, OrmError> {
        let compiled = compile_query(plan, &self.keys)?;
        Io {
            db: &self.db,
            session,
        }
        .query(&compiled)
        .await
    }

    async fn run_execute(
        &self,
        plan: &WritePlan,
        session: Option<&mut ClientSession>,
    ) -> Result<ExecResult, OrmError> {
        let compiled = compile_write(plan, &self.keys)?;
        Io {
            db: &self.db,
            session,
        }
        .write(compiled)
        .await
    }
}

fn raw_unsupported() -> OrmError {
    BackendCapabilityError::from_feature(BackendKind::MongoDb, Feature::RawSql).into()
}

#[async_trait]
impl Executor for MongoBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::mongodb()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        self.run_fetch(plan, None).await
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        self.run_execute(plan, None).await
    }

    async fn fetch_raw(&self, _sql: &str, _params: Vec<Value>) -> Result<QueryResult, OrmError> {
        Err(raw_unsupported())
    }

    async fn execute_raw(&self, _sql: &str, _params: Vec<Value>) -> Result<u64, OrmError> {
        Err(raw_unsupported())
    }

    async fn execute_script(&self, _sql: &str) -> Result<(), OrmError> {
        Err(raw_unsupported())
    }
}

#[async_trait]
impl Backend for MongoBackend {
    async fn begin(
        &self,
        isolation: Option<IsolationLevel>,
    ) -> Result<Box<dyn Transaction>, OrmError> {
        if let Some(level) = isolation {
            self.capabilities().require(Feature::Isolation(level))?;
        }
        let mut session = self
            .client
            .start_session()
            .await
            .map_err(|e| OrmError::from(map_error(&e)))?;
        session
            .start_transaction()
            .await
            .map_err(|e| OrmError::from(map_error(&e)))?;
        Ok(Box::new(MongoTransaction {
            backend: self.clone(),
            session: Mutex::new(Some(session)),
        }))
    }
}

/// An open MongoDB transaction. Dropped without commit, the driver aborts it.
struct MongoTransaction {
    backend: MongoBackend,
    session: Mutex<Option<ClientSession>>,
}

impl MongoTransaction {
    async fn take(&self) -> Result<ClientSession, QueryError> {
        self.session
            .lock()
            .await
            .take()
            .ok_or(QueryError::TransactionClosed)
    }
}

#[async_trait]
impl Executor for MongoTransaction {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::mongodb()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        // Compile before locking: unsupported plans fail without I/O.
        let compiled = compile_query(plan, &self.backend.keys)?;
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or(QueryError::TransactionClosed)?;
        Io {
            db: &self.backend.db,
            session: Some(session),
        }
        .query(&compiled)
        .await
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        let compiled = compile_write(plan, &self.backend.keys)?;
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or(QueryError::TransactionClosed)?;
        Io {
            db: &self.backend.db,
            session: Some(session),
        }
        .write(compiled)
        .await
    }

    async fn fetch_raw(&self, _sql: &str, _params: Vec<Value>) -> Result<QueryResult, OrmError> {
        Err(raw_unsupported())
    }

    async fn execute_raw(&self, _sql: &str, _params: Vec<Value>) -> Result<u64, OrmError> {
        Err(raw_unsupported())
    }

    async fn execute_script(&self, _sql: &str) -> Result<(), OrmError> {
        Err(raw_unsupported())
    }
}

#[async_trait]
impl Transaction for MongoTransaction {
    async fn commit(&self) -> Result<(), OrmError> {
        let mut session = self.take().await?;
        session
            .commit_transaction()
            .await
            .map_err(|e| map_error(&e).into())
    }

    async fn rollback(&self) -> Result<(), OrmError> {
        let mut session = self.take().await?;
        session
            .abort_transaction()
            .await
            .map_err(|e| map_error(&e).into())
    }
}

/// Classify a driver error. Duplicate-key and validation failures are
/// constraint violations; connection-level failures are connection errors.
fn map_error(e: &DriverError) -> BackendError {
    let message = e.to_string();
    if message.contains("E11000")
        || message.contains("code: 11000")
        || message.contains("code: 121")
    {
        return BackendError::Constraint(message);
    }
    match e.kind.as_ref() {
        ErrorKind::ServerSelection { .. }
        | ErrorKind::Io(_)
        | ErrorKind::DnsResolve { .. }
        | ErrorKind::Authentication { .. } => BackendError::Connection(message),
        _ => BackendError::Database(message),
    }
}

fn db_err(e: &DriverError) -> OrmError {
    map_error(e).into()
}

/// Rows of `doc`: the requested `columns`, or every field when empty
/// (`_id` renamed to `pk`).
fn doc_to_row(doc: Document, columns: &[String], pk: &str) -> Result<Row, QueryError> {
    if columns.is_empty() {
        let has_pk_field = doc.contains_key(pk);
        let mut out = Vec::with_capacity(doc.len());
        for (key, value) in doc {
            let name = if key == "_id" {
                if has_pk_field {
                    continue;
                }
                pk.to_owned()
            } else {
                key
            };
            let value = from_bson(value, &name)?;
            out.push((name, value));
        }
        return Ok(Row::new(out));
    }
    let mut out = Vec::with_capacity(columns.len());
    for column in columns {
        let key = if column == pk { "_id" } else { column.as_str() };
        let value = match doc.get(key) {
            Some(bson) => from_bson(bson.clone(), column)?,
            None => Value::Null,
        };
        out.push((column.clone(), value));
    }
    Ok(Row::new(out))
}

/// Database access for one statement, on a session or the implicit one.
struct Io<'a> {
    db: &'a Database,
    session: Option<&'a mut ClientSession>,
}

impl Io<'_> {
    fn coll(&self, name: &str) -> Collection<Document> {
        self.db.collection(name)
    }

    fn session(&mut self) -> Option<&mut ClientSession> {
        self.session.as_deref_mut()
    }

    async fn aggregate(
        &mut self,
        collection: &str,
        pipeline: Vec<Document>,
    ) -> Result<Vec<Document>, OrmError> {
        let coll = self.coll(collection);
        let mut out = Vec::new();
        match self.session() {
            Some(session) => {
                let mut cursor = coll
                    .aggregate(pipeline)
                    .session(&mut *session)
                    .await
                    .map_err(|e| db_err(&e))?;
                while cursor.advance(session).await.map_err(|e| db_err(&e))? {
                    out.push(cursor.deserialize_current().map_err(|e| db_err(&e))?);
                }
            }
            None => {
                let cursor = coll.aggregate(pipeline).await.map_err(|e| db_err(&e))?;
                out = drain(cursor).await?;
            }
        }
        Ok(out)
    }

    async fn find(
        &mut self,
        collection: &str,
        filter: Document,
        projection: Option<Document>,
    ) -> Result<Vec<Document>, OrmError> {
        let coll = self.coll(collection);
        let mut out = Vec::new();
        match self.session() {
            Some(session) => {
                let mut cursor = coll
                    .find(filter)
                    .optional(projection, |a, p| a.projection(p))
                    .session(&mut *session)
                    .await
                    .map_err(|e| db_err(&e))?;
                while cursor.advance(session).await.map_err(|e| db_err(&e))? {
                    out.push(cursor.deserialize_current().map_err(|e| db_err(&e))?);
                }
            }
            None => {
                let cursor = coll
                    .find(filter)
                    .optional(projection, |a, p| a.projection(p))
                    .await
                    .map_err(|e| db_err(&e))?;
                out = drain(cursor).await?;
            }
        }
        Ok(out)
    }

    async fn insert_many(&mut self, collection: &str, docs: Vec<Document>) -> Result<(), OrmError> {
        let coll = self.coll(collection);
        match self.session() {
            Some(session) => coll.insert_many(docs).session(session).await,
            None => coll.insert_many(docs).await,
        }
        .map(|_| ())
        .map_err(|e| db_err(&e))
    }

    async fn update_many(
        &mut self,
        collection: &str,
        filter: Document,
        update: UpdateSpec,
    ) -> Result<u64, OrmError> {
        let coll = self.coll(collection);
        let result = match (update, self.session()) {
            (UpdateSpec::Set(doc), Some(s)) => coll.update_many(filter, doc).session(s).await,
            (UpdateSpec::Set(doc), None) => coll.update_many(filter, doc).await,
            (UpdateSpec::Pipeline(p), Some(s)) => coll.update_many(filter, p).session(s).await,
            (UpdateSpec::Pipeline(p), None) => coll.update_many(filter, p).await,
        };
        result.map(|r| r.matched_count).map_err(|e| db_err(&e))
    }

    async fn delete_many(&mut self, collection: &str, filter: Document) -> Result<u64, OrmError> {
        let coll = self.coll(collection);
        match self.session() {
            Some(session) => coll.delete_many(filter).session(session).await,
            None => coll.delete_many(filter).await,
        }
        .map(|r| r.deleted_count)
        .map_err(|e| db_err(&e))
    }

    /// Reserve `n` consecutive integer keys for `collection`; returns the
    /// first. Runs outside the transaction (like a sequence) so concurrent
    /// transactions never conflict on the counter; a rolled-back
    /// transaction leaves a gap.
    async fn reserve_keys(&self, collection: &str, n: usize) -> Result<i64, OrmError> {
        let n = i64::try_from(n).map_err(|_| QueryError::InvalidPlan("too many rows".into()))?;
        let counters: Collection<Document> = self.db.collection(COUNTERS);
        let after = counters
            .find_one_and_update(doc! { "_id": collection }, doc! { "$inc": { "seq": n } })
            .upsert(true)
            .return_document(ReturnDocument::After)
            .await
            .map_err(|e| db_err(&e))?
            .and_then(|d| d.get("seq").and_then(as_i64))
            .ok_or_else(|| BackendError::Database("key counter has no `seq`".into()))?;
        Ok(after - n + 1)
    }

    async fn query(&mut self, compiled: &CompiledQuery) -> Result<QueryResult, OrmError> {
        let docs = self
            .aggregate(&compiled.collection, compiled.pipeline.clone())
            .await?;
        if docs.is_empty()
            && let Some(values) = &compiled.empty_row
        {
            let row = compiled
                .columns
                .iter()
                .cloned()
                .zip(values.iter().cloned())
                .collect();
            return Ok(QueryResult {
                rows: vec![Row::new(row)],
            });
        }
        let rows = docs
            .into_iter()
            .map(|d| doc_to_row(d, &compiled.columns, &compiled.pk))
            .collect::<Result<_, _>>()?;
        Ok(QueryResult { rows })
    }

    async fn write(&mut self, compiled: CompiledWrite) -> Result<ExecResult, OrmError> {
        match compiled {
            CompiledWrite::Insert(p) => self.insert(p).await,
            CompiledWrite::Update(p) => self.update(p).await,
            CompiledWrite::Delete(p) => self.delete(p).await,
        }
    }

    async fn insert(&mut self, plan: CompiledInsert) -> Result<ExecResult, OrmError> {
        let CompiledInsert {
            collection,
            pk,
            docs,
            returning,
        } = plan;
        if docs.is_empty() {
            return Ok(ExecResult::default());
        }
        let missing = docs.iter().filter(|d| !d.contains_key("_id")).count();
        let mut next = if missing > 0 {
            self.reserve_keys(&collection, missing).await?
        } else {
            0
        };
        let docs: Vec<Document> = docs
            .into_iter()
            .map(|doc| {
                if doc.contains_key("_id") {
                    return doc;
                }
                // `_id` first, as MongoDB stores it.
                let mut with_id = doc! { "_id": next };
                next += 1;
                with_id.extend(doc);
                with_id
            })
            .collect();
        self.insert_many(&collection, docs.clone()).await?;
        let mut result = ExecResult {
            rows_affected: docs.len() as u64,
            returning: Vec::new(),
        };
        if !returning.is_empty() {
            // Echo what was stored (values as BSON holds them).
            result.returning = docs
                .into_iter()
                .map(|d| doc_to_row(d, &returning, &pk))
                .collect::<Result<_, _>>()?;
        }
        Ok(result)
    }

    async fn update(&mut self, plan: CompiledUpdate) -> Result<ExecResult, OrmError> {
        let CompiledUpdate {
            collection,
            pk,
            filter,
            update,
            returning,
        } = plan;
        if returning.is_empty() {
            let matched = self.update_many(&collection, filter, update).await?;
            return Ok(ExecResult {
                rows_affected: matched,
                returning: Vec::new(),
            });
        }
        // Pin the rows first, so the read-back is not re-matched against
        // the updated values.
        let ids = self.matching_ids(&collection, filter).await?;
        if ids.is_empty() {
            return Ok(ExecResult::default());
        }
        let by_id = doc! { "_id": { "$in": ids } };
        let matched = self.update_many(&collection, by_id.clone(), update).await?;
        let rows = self
            .find(&collection, by_id, None)
            .await?
            .into_iter()
            .map(|d| doc_to_row(d, &returning, &pk))
            .collect::<Result<_, _>>()?;
        Ok(ExecResult {
            rows_affected: matched,
            returning: rows,
        })
    }

    async fn delete(&mut self, plan: CompiledDelete) -> Result<ExecResult, OrmError> {
        let CompiledDelete {
            collection,
            pk,
            filter,
            returning,
        } = plan;
        if returning.is_empty() {
            let deleted = self.delete_many(&collection, filter).await?;
            return Ok(ExecResult {
                rows_affected: deleted,
                returning: Vec::new(),
            });
        }
        let docs = self.find(&collection, filter, None).await?;
        let ids: Vec<Bson> = docs.iter().filter_map(|d| d.get("_id").cloned()).collect();
        if ids.is_empty() {
            return Ok(ExecResult::default());
        }
        let deleted = self
            .delete_many(&collection, doc! { "_id": { "$in": ids } })
            .await?;
        let rows = docs
            .into_iter()
            .map(|d| doc_to_row(d, &returning, &pk))
            .collect::<Result<_, _>>()?;
        Ok(ExecResult {
            rows_affected: deleted,
            returning: rows,
        })
    }

    async fn matching_ids(
        &mut self,
        collection: &str,
        filter: Document,
    ) -> Result<Vec<Bson>, OrmError> {
        Ok(self
            .find(collection, filter, Some(doc! { "_id": 1_i32 }))
            .await?
            .into_iter()
            .filter_map(|d| d.get("_id").cloned())
            .collect())
    }
}

fn as_i64(value: &Bson) -> Option<i64> {
    match value {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        _ => None,
    }
}

async fn drain(mut cursor: Cursor<Document>) -> Result<Vec<Document>, OrmError> {
    let mut out = Vec::new();
    while cursor.advance().await.map_err(|e| db_err(&e))? {
        out.push(cursor.deserialize_current().map_err(|e| db_err(&e))?);
    }
    Ok(out)
}
