mod sqlite;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlite::{Connection, Step};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Compatibility {
    system: String,
    model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ProductInput {
    id: String,
    category: String,
    name: String,
    price_cents: i64,
    #[serde(default)]
    description: String,
    specs: Value,
    #[serde(default)]
    compatibility: Vec<Compatibility>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Product {
    id: String,
    version: i64,
    category: String,
    name: String,
    price_cents: i64,
    #[serde(default)]
    description: String,
    spec_version: i64,
    specs: Value,
    compatibility: Vec<Compatibility>,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProductPatch {
    expected_version: i64,
    name: Option<String>,
    price_cents: Option<i64>,
    description: Option<String>,
    specs: Option<Value>,
    compatibility: Option<Vec<Compatibility>>,
    tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApiError {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

#[derive(Debug)]
struct Failure {
    status: StatusCode,
    body: ApiError,
}

type Result<T> = std::result::Result<T, Failure>;

impl Failure {
    fn invalid(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            body: ApiError {
                error: message.into(),
                path: Some(path.into()),
            },
        }
    }

    fn not_found(id: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            body: ApiError {
                error: format!("product {id} not found"),
                path: None,
            },
        }
    }

    fn conflict(expected: i64, actual: i64) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: ApiError {
                error: format!("stale version: expected {expected}, current is {actual}"),
                path: Some("expected_version".into()),
            },
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: ApiError {
                error: error.to_string(),
                path: None,
            },
        }
    }
}

impl From<sqlite::Error> for Failure {
    fn from(value: sqlite::Error) -> Self {
        Self::internal(value)
    }
}

type HttpError = (StatusCode, Json<ApiError>);

impl From<Failure> for HttpError {
    fn from(value: Failure) -> Self {
        (value.status, Json(value.body))
    }
}

struct Catalog {
    db: Connection,
}

impl Catalog {
    fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS category_schemas (
               category TEXT NOT NULL, revision INTEGER NOT NULL,
               active INTEGER NOT NULL DEFAULT 0, schema_json TEXT NOT NULL,
               PRIMARY KEY(category, revision)
             );
             CREATE UNIQUE INDEX IF NOT EXISTS one_active_schema
               ON category_schemas(category) WHERE active=1;
             CREATE TABLE IF NOT EXISTS products (
               id TEXT PRIMARY KEY, version INTEGER NOT NULL,
               category TEXT NOT NULL, name TEXT NOT NULL,
               price_cents INTEGER NOT NULL, description TEXT NOT NULL DEFAULT '',
               spec_version INTEGER NOT NULL, specs_json TEXT NOT NULL,
               compatibility_json TEXT NOT NULL, tags_json TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS products_category ON products(category);
             CREATE INDEX IF NOT EXISTS products_price ON products(price_cents);
             CREATE TABLE IF NOT EXISTS product_facets (
               product_id TEXT NOT NULL REFERENCES products(id) ON DELETE CASCADE,
               path TEXT NOT NULL, text_value TEXT, num_value REAL,
               PRIMARY KEY(product_id, path)
             );
             CREATE INDEX IF NOT EXISTS facets_text ON product_facets(path, text_value, product_id);
             CREATE INDEX IF NOT EXISTS facets_num ON product_facets(path, num_value, product_id);
             CREATE TABLE IF NOT EXISTS import_jobs (
               job_id TEXT PRIMARY KEY, next_ordinal INTEGER NOT NULL,
               total INTEGER NOT NULL, source_fingerprint TEXT NOT NULL,
               completed INTEGER NOT NULL DEFAULT 0
             );",
        )?;
        Ok(Self { db })
    }

    fn install_demo_schemas(&self, laptop_revision: i64) -> Result<()> {
        for (category, revision, schema) in [
            (
                "laptop",
                1,
                r#"{"ram_gb":"integer 4..128","screen_inches":"number 10..20"}"#,
            ),
            (
                "laptop",
                2,
                r#"{"ram_gb":"integer 4..128","screen_inches":"number 10..20","battery_wh":"integer 20..150"}"#,
            ),
            (
                "cable",
                1,
                r#"{"length_m":"number 0..100","connector":"usb-c|hdmi|ethernet"}"#,
            ),
            (
                "chair",
                1,
                r#"{"max_weight_kg":"integer 40..300","adjustable":"boolean"}"#,
            ),
        ] {
            let mut statement = self.db.prepare(
                "INSERT OR IGNORE INTO category_schemas(category,revision,active,schema_json)
                 VALUES(?,?,0,?)",
            )?;
            statement.bind_text(1, category)?;
            statement.bind_i64(2, revision)?;
            statement.bind_text(3, schema)?;
            statement.execute()?;
        }
        for category in ["laptop", "cable", "chair"] {
            let revision = if category == "laptop" {
                laptop_revision
            } else {
                1
            };
            self.activate_schema(category, revision)?;
        }
        Ok(())
    }

    fn activate_schema(&self, category: &str, revision: i64) -> Result<()> {
        self.db.transaction(|| {
            let mut off = self
                .db
                .prepare("UPDATE category_schemas SET active=0 WHERE category=?")?;
            off.bind_text(1, category)?;
            off.execute()?;
            let mut on = self
                .db
                .prepare("UPDATE category_schemas SET active=1 WHERE category=? AND revision=?")?;
            on.bind_text(1, category)?;
            on.bind_i64(2, revision)?;
            on.execute()?;
            if self.db.changes() != 1 {
                return Err(sqlite::Error(format!(
                    "missing schema {category} revision {revision}"
                )));
            }
            Ok(())
        })?;
        Ok(())
    }

    fn active_revision(&self, category: &str) -> Result<i64> {
        let mut statement = self
            .db
            .prepare("SELECT revision FROM category_schemas WHERE category=? AND active=1")?;
        statement.bind_text(1, category)?;
        match statement.step()? {
            Step::Row => Ok(statement.column_i64(0)),
            Step::Done => Err(Failure::invalid("category", "unknown category")),
        }
    }

    fn create(&self, input: ProductInput) -> Result<Product> {
        let revision = self.active_revision(&input.category)?;
        self.create_at_revision(input, revision)
    }

    fn create_at_revision(&self, input: ProductInput, revision: i64) -> Result<Product> {
        validate_input(&input, revision)?;
        let product = Product {
            id: input.id,
            version: 1,
            category: input.category,
            name: input.name,
            price_cents: input.price_cents,
            description: input.description,
            spec_version: revision,
            specs: input.specs,
            compatibility: input.compatibility,
            tags: input.tags,
        };
        self.db
            .transaction(|| self.insert_raw(&product, false).map(|_| ()))?;
        Ok(product)
    }

    fn insert_raw(&self, product: &Product, ignore_duplicate: bool) -> sqlite::Result<bool> {
        let verb = if ignore_duplicate { "OR IGNORE " } else { "" };
        let sql = format!(
            "INSERT {verb}INTO products
             (id,version,category,name,price_cents,description,spec_version,specs_json,compatibility_json,tags_json)
             VALUES(?,?,?,?,?,?,?,?,?,?)"
        );
        let mut statement = self.db.prepare(&sql)?;
        statement.bind_text(1, &product.id)?;
        statement.bind_i64(2, product.version)?;
        statement.bind_text(3, &product.category)?;
        statement.bind_text(4, &product.name)?;
        statement.bind_i64(5, product.price_cents)?;
        statement.bind_text(6, &product.description)?;
        statement.bind_i64(7, product.spec_version)?;
        statement.bind_text(8, &serde_json::to_string(&product.specs).unwrap())?;
        statement.bind_text(9, &serde_json::to_string(&product.compatibility).unwrap())?;
        statement.bind_text(10, &serde_json::to_string(&product.tags).unwrap())?;
        statement.execute()?;
        let inserted = self.db.changes() == 1;
        if !ignore_duplicate || inserted {
            self.replace_facets(product)?;
        }
        Ok(inserted)
    }

    fn replace_facets(&self, product: &Product) -> sqlite::Result<()> {
        let mut delete = self
            .db
            .prepare("DELETE FROM product_facets WHERE product_id=?")?;
        delete.bind_text(1, &product.id)?;
        delete.execute()?;
        let object = product.specs.as_object().expect("validated specs object");
        for (name, value) in object {
            let path = format!("specs.{name}");
            let mut insert = self.db.prepare(
                "INSERT INTO product_facets(product_id,path,text_value,num_value)
                 VALUES(?,?,?,?)",
            )?;
            insert.bind_text(1, &product.id)?;
            insert.bind_text(2, &path)?;
            match value {
                Value::String(text) => {
                    insert.bind_text(3, text)?;
                    insert.bind_null(4)?;
                }
                Value::Number(number) => {
                    insert.bind_null(3)?;
                    insert.bind_f64(4, number.as_f64().unwrap())?;
                }
                Value::Bool(value) => {
                    insert.bind_text(3, if *value { "true" } else { "false" })?;
                    insert.bind_null(4)?;
                }
                _ => continue,
            }
            insert.execute()?;
        }
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Product> {
        self.get_raw(id)?.ok_or_else(|| Failure::not_found(id))
    }

    fn get_raw(&self, id: &str) -> sqlite::Result<Option<Product>> {
        let mut statement = self.db.prepare(
            "SELECT id,version,category,name,price_cents,description,spec_version,
                    specs_json,compatibility_json,tags_json FROM products WHERE id=?",
        )?;
        statement.bind_text(1, id)?;
        match statement.step()? {
            Step::Row => row_product(&statement).map(Some).map_err(sqlite::Error),
            Step::Done => Ok(None),
        }
    }

    fn patch(&self, id: &str, patch: ProductPatch) -> Result<Product> {
        self.db
            .transaction(|| {
                let mut product = self.get(id).map_err(|e| sqlite::Error(e.body.error))?;
                if product.version != patch.expected_version {
                    return Err(sqlite::Error(format!(
                        "STALE:{}:{}",
                        patch.expected_version, product.version
                    )));
                }
                if let Some(name) = patch.name {
                    product.name = name;
                }
                if let Some(price) = patch.price_cents {
                    product.price_cents = price;
                }
                if let Some(description) = patch.description {
                    product.description = description;
                }
                if let Some(spec_patch) = patch.specs {
                    merge_specs(&mut product.specs, spec_patch).map_err(|e| {
                        sqlite::Error(format!(
                            "INVALID:{}:{}",
                            e.body.path.unwrap_or_default(),
                            e.body.error
                        ))
                    })?;
                }
                if let Some(compatibility) = patch.compatibility {
                    product.compatibility = compatibility;
                }
                if let Some(tags) = patch.tags {
                    product.tags = tags;
                }
                validate_product(&product).map_err(|e| {
                    sqlite::Error(format!(
                        "INVALID:{}:{}",
                        e.body.path.unwrap_or_default(),
                        e.body.error
                    ))
                })?;
                product.version += 1;
                let mut update = self.db.prepare(
                    "UPDATE products SET version=?,name=?,price_cents=?,description=?,specs_json=?,
                 compatibility_json=?,tags_json=? WHERE id=? AND version=?",
                )?;
                update.bind_i64(1, product.version)?;
                update.bind_text(2, &product.name)?;
                update.bind_i64(3, product.price_cents)?;
                update.bind_text(4, &product.description)?;
                update.bind_text(5, &serde_json::to_string(&product.specs).unwrap())?;
                update.bind_text(6, &serde_json::to_string(&product.compatibility).unwrap())?;
                update.bind_text(7, &serde_json::to_string(&product.tags).unwrap())?;
                update.bind_text(8, id)?;
                update.bind_i64(9, patch.expected_version)?;
                update.execute()?;
                if self.db.changes() != 1 {
                    return Err(sqlite::Error("concurrent update lost race".into()));
                }
                self.replace_facets(&product)?;
                Ok(product)
            })
            .map_err(|error| decode_transaction_error(error, id))
    }

    fn delete(&self, id: &str, expected_version: i64) -> Result<Product> {
        let product = self.get(id)?;
        if product.version != expected_version {
            return Err(Failure::conflict(expected_version, product.version));
        }
        let mut statement = self
            .db
            .prepare("DELETE FROM products WHERE id=? AND version=?")?;
        statement.bind_text(1, id)?;
        statement.bind_i64(2, expected_version)?;
        statement.execute()?;
        if self.db.changes() != 1 {
            return Err(Failure::conflict(expected_version, self.get(id)?.version));
        }
        Ok(product)
    }

    fn migrate_laptop_v1_to_v2(&self, id: &str, default_battery_wh: i64) -> Result<Product> {
        let mut product = self.get(id)?;
        if product.category != "laptop" || product.spec_version != 1 {
            return Err(Failure::invalid(
                "spec_version",
                "expected laptop revision 1",
            ));
        }
        product
            .specs
            .as_object_mut()
            .unwrap()
            .insert("battery_wh".into(), json!(default_battery_wh));
        product.spec_version = 2;
        validate_product(&product)?;
        let old_version = product.version;
        product.version += 1;
        self.db.transaction(|| {
            let mut update = self.db.prepare(
                "UPDATE products SET version=?,spec_version=2,specs_json=? WHERE id=? AND version=?",
            )?;
            update.bind_i64(1, product.version)?;
            update.bind_text(2, &serde_json::to_string(&product.specs).unwrap())?;
            update.bind_text(3, id)?;
            update.bind_i64(4, old_version)?;
            update.execute()?;
            if self.db.changes() != 1 {
                return Err(sqlite::Error("migration version race".into()));
            }
            self.replace_facets(&product)
        })?;
        Ok(product)
    }

    fn filter(&self, query: &FilterQuery) -> Result<Vec<Product>> {
        let mut sql = String::from(
            "SELECT id,version,category,name,price_cents,description,spec_version,
             specs_json,compatibility_json,tags_json FROM products p WHERE 1=1",
        );
        if query.category.is_some() {
            sql.push_str(" AND p.category=?");
        }
        if query.exact_path.is_some() {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM product_facets f WHERE f.product_id=p.id
                   AND f.path=? AND (f.text_value=? OR f.num_value=?))",
            );
        }
        if query.range_path.is_some() {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM product_facets r WHERE r.product_id=p.id
                   AND r.path=? AND (? IS NULL OR r.num_value>=?)
                   AND (? IS NULL OR r.num_value<=?))",
            );
        }
        sql.push_str(" ORDER BY p.id");
        let mut statement = self.db.prepare(&sql)?;
        let mut index = 1;
        if let Some(category) = &query.category {
            statement.bind_text(index, category)?;
            index += 1;
        }
        if let Some(path) = &query.exact_path {
            let value = query.exact_value.as_deref().unwrap_or("");
            statement.bind_text(index, path)?;
            statement.bind_text(index + 1, value)?;
            if let Ok(number) = value.parse::<f64>() {
                statement.bind_f64(index + 2, number)?;
            } else {
                statement.bind_null(index + 2)?;
            }
            index += 3;
        }
        if let Some(path) = &query.range_path {
            statement.bind_text(index, path)?;
            bind_optional_f64(&mut statement, index + 1, query.min)?;
            bind_optional_f64(&mut statement, index + 2, query.min)?;
            bind_optional_f64(&mut statement, index + 3, query.max)?;
            bind_optional_f64(&mut statement, index + 4, query.max)?;
        }
        let mut products = Vec::new();
        while matches!(statement.step()?, Step::Row) {
            products.push(row_product(&statement).map_err(Failure::internal)?);
        }
        Ok(products)
    }

    fn all(&self) -> Result<Vec<Product>> {
        self.filter(&FilterQuery::default())
    }

    fn resume_import(
        &self,
        job_id: &str,
        total: usize,
        batch_size: usize,
        stop_after_batches: Option<usize>,
        payload_bytes: usize,
    ) -> Result<ImportProgress> {
        let source_fingerprint = format!("catalog-seed:v1:payload-bytes={payload_bytes}");
        let mut progress = self.import_progress(job_id)?.unwrap_or(ImportProgress {
            next_ordinal: 0,
            total,
            source_fingerprint: source_fingerprint.clone(),
            completed: false,
        });
        if progress.total != total {
            return Err(Failure::invalid("total", "import total changed on resume"));
        }
        if progress.source_fingerprint != source_fingerprint {
            return Err(Failure::invalid(
                "source_fingerprint",
                "import source changed on resume",
            ));
        }
        let mut batches = 0;
        while progress.next_ordinal < total {
            let end = (progress.next_ordinal + batch_size).min(total);
            self.db.transaction(|| {
                for ordinal in progress.next_ordinal..end {
                    let product = generated_product(ordinal, payload_bytes);
                    if !self.insert_raw(&product, true)?
                        && self.get_raw(&product.id)?.as_ref() != Some(&product)
                    {
                        return Err(sqlite::Error(format!(
                            "import product {} conflicts with existing content",
                            product.id
                        )));
                    }
                }
                let mut checkpoint = self.db.prepare(
                    "INSERT INTO import_jobs
                     (job_id,next_ordinal,total,source_fingerprint,completed) VALUES(?,?,?,?,?)
                     ON CONFLICT(job_id) DO UPDATE SET next_ordinal=excluded.next_ordinal,
                     total=excluded.total,source_fingerprint=excluded.source_fingerprint,
                     completed=excluded.completed",
                )?;
                checkpoint.bind_text(1, job_id)?;
                checkpoint.bind_i64(2, end as i64)?;
                checkpoint.bind_i64(3, total as i64)?;
                checkpoint.bind_text(4, &source_fingerprint)?;
                checkpoint.bind_i64(5, i64::from(end == total))?;
                checkpoint.execute()
            })?;
            progress.next_ordinal = end;
            progress.completed = end == total;
            batches += 1;
            if stop_after_batches == Some(batches) {
                break;
            }
        }
        Ok(progress)
    }

    fn import_progress(&self, job_id: &str) -> Result<Option<ImportProgress>> {
        let mut statement = self.db.prepare(
            "SELECT next_ordinal,total,source_fingerprint,completed
                 FROM import_jobs WHERE job_id=?",
        )?;
        statement.bind_text(1, job_id)?;
        match statement.step()? {
            Step::Row => Ok(Some(ImportProgress {
                next_ordinal: statement.column_i64(0) as usize,
                total: statement.column_i64(1) as usize,
                source_fingerprint: statement.column_text(2),
                completed: statement.column_i64(3) != 0,
            })),
            Step::Done => Ok(None),
        }
    }

    fn count_products(&self) -> Result<(usize, usize)> {
        let mut statement = self
            .db
            .prepare("SELECT COUNT(*),COUNT(DISTINCT id) FROM products")?;
        statement.step()?;
        Ok((
            statement.column_i64(0) as usize,
            statement.column_i64(1) as usize,
        ))
    }

    fn checkpoint(&self) -> Result<()> {
        self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }
}

fn decode_transaction_error(error: sqlite::Error, id: &str) -> Failure {
    if let Some(rest) = error.0.strip_prefix("STALE:") {
        let mut numbers = rest.split(':').filter_map(|n| n.parse().ok());
        return Failure::conflict(numbers.next().unwrap_or(0), numbers.next().unwrap_or(0));
    }
    if let Some(rest) = error.0.strip_prefix("INVALID:") {
        let (path, message) = rest.split_once(':').unwrap_or(("specs", rest));
        return Failure::invalid(path, message);
    }
    if error.0.contains("not found") {
        return Failure::not_found(id);
    }
    Failure::from(error)
}

fn bind_optional_f64(
    statement: &mut sqlite::Statement,
    index: i32,
    value: Option<f64>,
) -> sqlite::Result<()> {
    match value {
        Some(value) => statement.bind_f64(index, value),
        None => statement.bind_null(index),
    }
}

fn row_product(statement: &sqlite::Statement) -> std::result::Result<Product, String> {
    Ok(Product {
        id: statement.column_text(0),
        version: statement.column_i64(1),
        category: statement.column_text(2),
        name: statement.column_text(3),
        price_cents: statement.column_i64(4),
        description: statement.column_text(5),
        spec_version: statement.column_i64(6),
        specs: serde_json::from_str(&statement.column_text(7)).map_err(|e| e.to_string())?,
        compatibility: serde_json::from_str(&statement.column_text(8))
            .map_err(|e| e.to_string())?,
        tags: serde_json::from_str(&statement.column_text(9)).map_err(|e| e.to_string())?,
    })
}

fn validate_input(input: &ProductInput, revision: i64) -> Result<()> {
    if input.id.trim().is_empty() {
        return Err(Failure::invalid("id", "must not be empty"));
    }
    let product = Product {
        id: input.id.clone(),
        version: 1,
        category: input.category.clone(),
        name: input.name.clone(),
        price_cents: input.price_cents,
        description: input.description.clone(),
        spec_version: revision,
        specs: input.specs.clone(),
        compatibility: input.compatibility.clone(),
        tags: input.tags.clone(),
    };
    validate_product(&product)
}

fn validate_product(product: &Product) -> Result<()> {
    if product.name.trim().is_empty() {
        return Err(Failure::invalid("name", "must not be empty"));
    }
    if product.price_cents < 0 {
        return Err(Failure::invalid("price_cents", "must be non-negative"));
    }
    if product.tags.len() > 32 {
        return Err(Failure::invalid("tags", "must contain at most 32 tags"));
    }
    for (index, entry) in product.compatibility.iter().enumerate() {
        if entry.system.trim().is_empty() || entry.model.trim().is_empty() {
            return Err(Failure::invalid(
                format!("compatibility[{index}]"),
                "system and model must not be empty",
            ));
        }
    }
    let specs = product
        .specs
        .as_object()
        .ok_or_else(|| Failure::invalid("specs", "must be an object"))?;
    match (product.category.as_str(), product.spec_version) {
        ("laptop", 1) => {
            integer_in(specs, "ram_gb", 4, 128)?;
            number_in(specs, "screen_inches", 10.0, 20.0)?;
            reject_unknown(specs, &["ram_gb", "screen_inches"])?;
        }
        ("laptop", 2) => {
            integer_in(specs, "ram_gb", 4, 128)?;
            number_in(specs, "screen_inches", 10.0, 20.0)?;
            integer_in(specs, "battery_wh", 20, 150)?;
            reject_unknown(specs, &["ram_gb", "screen_inches", "battery_wh"])?;
        }
        ("cable", 1) => {
            number_in(specs, "length_m", 0.01, 100.0)?;
            let connector = specs
                .get("connector")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::invalid("specs.connector", "must be a string"))?;
            if !["usb-c", "hdmi", "ethernet"].contains(&connector) {
                return Err(Failure::invalid("specs.connector", "unsupported connector"));
            }
            reject_unknown(specs, &["length_m", "connector"])?;
        }
        ("chair", 1) => {
            integer_in(specs, "max_weight_kg", 40, 300)?;
            if !specs.get("adjustable").is_some_and(Value::is_boolean) {
                return Err(Failure::invalid("specs.adjustable", "must be a boolean"));
            }
            reject_unknown(specs, &["max_weight_kg", "adjustable"])?;
        }
        _ => {
            return Err(Failure::invalid(
                "spec_version",
                "unsupported schema revision",
            ));
        }
    }
    Ok(())
}

fn integer_in(specs: &Map<String, Value>, key: &str, min: i64, max: i64) -> Result<()> {
    let value = specs
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| Failure::invalid(format!("specs.{key}"), "must be an integer"))?;
    if !(min..=max).contains(&value) {
        return Err(Failure::invalid(
            format!("specs.{key}"),
            format!("must be between {min} and {max}"),
        ));
    }
    Ok(())
}

fn number_in(specs: &Map<String, Value>, key: &str, min: f64, max: f64) -> Result<()> {
    let value = specs
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| Failure::invalid(format!("specs.{key}"), "must be a number"))?;
    if !(min..=max).contains(&value) {
        return Err(Failure::invalid(
            format!("specs.{key}"),
            format!("must be between {min} and {max}"),
        ));
    }
    Ok(())
}

fn reject_unknown(specs: &Map<String, Value>, allowed: &[&str]) -> Result<()> {
    if let Some(key) = specs.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(Failure::invalid(
            format!("specs.{key}"),
            "field is not in this schema revision",
        ));
    }
    Ok(())
}

fn merge_specs(target: &mut Value, patch: Value) -> Result<()> {
    let patch = patch
        .as_object()
        .ok_or_else(|| Failure::invalid("specs", "patch must be an object"))?;
    let target = target.as_object_mut().expect("stored specs are validated");
    for (key, value) in patch {
        target.insert(key.clone(), value.clone());
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Default)]
struct FilterQuery {
    category: Option<String>,
    exact_path: Option<String>,
    exact_value: Option<String>,
    range_path: Option<String>,
    min: Option<f64>,
    max: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeleteQuery {
    expected_version: i64,
}

#[derive(Debug, Clone)]
struct ImportProgress {
    next_ordinal: usize,
    total: usize,
    source_fingerprint: String,
    completed: bool,
}

type Shared = Arc<Mutex<Catalog>>;

fn app(catalog: Catalog) -> Router {
    Router::new()
        .route("/products", post(http_create).get(http_filter))
        .route(
            "/products/{id}",
            get(http_get).patch(http_patch).delete(http_delete),
        )
        .with_state(Arc::new(Mutex::new(catalog)))
}

async fn http_create(
    State(state): State<Shared>,
    Json(input): Json<ProductInput>,
) -> std::result::Result<(StatusCode, Json<Product>), HttpError> {
    let product = state
        .lock()
        .unwrap()
        .create(input)
        .map_err(HttpError::from)?;
    Ok((StatusCode::CREATED, Json(product)))
}

async fn http_get(
    State(state): State<Shared>,
    AxumPath(id): AxumPath<String>,
) -> std::result::Result<Json<Product>, HttpError> {
    state
        .lock()
        .unwrap()
        .get(&id)
        .map(Json)
        .map_err(HttpError::from)
}

async fn http_patch(
    State(state): State<Shared>,
    AxumPath(id): AxumPath<String>,
    Json(patch): Json<ProductPatch>,
) -> std::result::Result<Json<Product>, HttpError> {
    state
        .lock()
        .unwrap()
        .patch(&id, patch)
        .map(Json)
        .map_err(HttpError::from)
}

async fn http_delete(
    State(state): State<Shared>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<DeleteQuery>,
) -> std::result::Result<Json<Product>, HttpError> {
    state
        .lock()
        .unwrap()
        .delete(&id, query.expected_version)
        .map(Json)
        .map_err(HttpError::from)
}

async fn http_filter(
    State(state): State<Shared>,
    Query(query): Query<FilterQuery>,
) -> std::result::Result<Json<Vec<Product>>, HttpError> {
    state
        .lock()
        .unwrap()
        .filter(&query)
        .map(Json)
        .map_err(HttpError::from)
}

fn generated_product(ordinal: usize, payload_bytes: usize) -> Product {
    let category = match ordinal % 3 {
        0 => "laptop",
        1 => "cable",
        _ => "chair",
    };
    let ram_gb = [8, 16, 32, 64][(ordinal / 3) % 4];
    let connector = ["usb-c", "hdmi", "ethernet"][(ordinal / 3) % 3];
    let specs = match category {
        "laptop" => json!({
            "ram_gb": ram_gb,
            "screen_inches": 13.0 + (ordinal % 4) as f64,
            "battery_wh": 40 + (ordinal % 80) as i64
        }),
        "cable" => json!({
            "length_m": 0.5 + (ordinal % 20) as f64 * 0.5,
            "connector": connector
        }),
        _ => json!({
            "max_weight_kg": 80 + (ordinal % 140) as i64,
            "adjustable": ordinal.is_multiple_of(2)
        }),
    };
    Product {
        id: format!("bulk-{ordinal:06}"),
        version: 1,
        category: category.into(),
        name: format!("Seed product {ordinal}"),
        price_cents: 1_000 + (ordinal % 100_000) as i64,
        description: seeded_text(ordinal as u64, payload_bytes),
        spec_version: if category == "laptop" { 2 } else { 1 },
        specs,
        compatibility: vec![Compatibility {
            system: "catalog".into(),
            model: format!("m-{}", ordinal % 50),
        }],
        tags: vec![format!("tag-{}", ordinal % 20)],
    }
}

fn seeded_text(mut state: u64, bytes: usize) -> String {
    let mut output = String::with_capacity(bytes);
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    while output.len() < bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        output.push(ALPHABET[(state as usize) % ALPHABET.len()] as char);
    }
    output
}

fn reference_filter(products: &[Product], query: &FilterQuery) -> Vec<String> {
    let mut ids = products
        .iter()
        .filter(|product| {
            query
                .category
                .as_ref()
                .is_none_or(|category| product.category == *category)
        })
        .filter(|product| {
            query.exact_path.as_ref().is_none_or(|path| {
                let value = query.exact_value.as_deref().unwrap_or("");
                json_path(&product.specs, path).is_some_and(|candidate| match candidate {
                    Value::String(text) => text == value,
                    Value::Number(number) => value
                        .parse::<f64>()
                        .ok()
                        .is_some_and(|v| number.as_f64() == Some(v)),
                    Value::Bool(flag) => value == flag.to_string(),
                    _ => false,
                })
            })
        })
        .filter(|product| {
            query.range_path.as_ref().is_none_or(|path| {
                json_path(&product.specs, path)
                    .and_then(Value::as_f64)
                    .is_some_and(|value| {
                        query.min.is_none_or(|min| value >= min)
                            && query.max.is_none_or(|max| value <= max)
                    })
            })
        })
        .map(|product| product.id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn json_path<'a>(specs: &'a Value, path: &str) -> Option<&'a Value> {
    let key = path.strip_prefix("specs.")?;
    specs.get(key)
}

fn assert_reference(catalog: &Catalog, query: FilterQuery) -> Result<usize> {
    let expected = reference_filter(&catalog.all()?, &query);
    let actual = catalog
        .filter(&query)?
        .into_iter()
        .map(|product| product.id)
        .collect::<Vec<_>>();
    if actual != expected {
        return Err(Failure::internal(format!(
            "filter mismatch: actual={actual:?}, expected={expected:?}"
        )));
    }
    Ok(actual.len())
}

fn example_input(id: &str, category: &str) -> ProductInput {
    let specs = match category {
        "laptop" => json!({"ram_gb":16,"screen_inches":14.0,"battery_wh":70}),
        "cable" => json!({"length_m":2.0,"connector":"usb-c"}),
        _ => json!({"max_weight_kg":120,"adjustable":true}),
    };
    ProductInput {
        id: id.into(),
        category: category.into(),
        name: format!("Example {category}"),
        price_cents: 12_500,
        description: "representative product".into(),
        specs,
        compatibility: vec![Compatibility {
            system: "inventory".into(),
            model: "v1".into(),
        }],
        tags: vec!["wholesale".into()],
    }
}

fn fresh_path(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/trial-data");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(name);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
    path
}

fn run_demo() -> Result<()> {
    let path = fresh_path("demo.sqlite");
    {
        let legacy = Connection::open(&path)?;
        legacy.execute_batch("CREATE TABLE legacy_orders(id INTEGER PRIMARY KEY); INSERT INTO legacy_orders VALUES(7);")?;
    }
    let catalog = Catalog::open(&path)?;
    catalog.install_demo_schemas(1)?;
    let mut old = example_input("legacy-laptop", "laptop");
    old.specs = json!({"ram_gb":16,"screen_inches":14.0});
    catalog.create_at_revision(old, 1)?;
    catalog.activate_schema("laptop", 2)?;
    catalog.create(example_input("cable-1", "cable"))?;
    catalog.create(example_input("chair-1", "chair"))?;
    let before = catalog.get("legacy-laptop")?;
    let migrated = catalog.migrate_laptop_v1_to_v2("legacy-laptop", 60)?;
    let patched = catalog.patch(
        "cable-1",
        ProductPatch {
            expected_version: 1,
            specs: Some(json!({"length_m":3.5})),
            ..ProductPatch::default()
        },
    )?;
    let stale_rejected = catalog
        .patch(
            "cable-1",
            ProductPatch {
                expected_version: 1,
                name: Some("stale".into()),
                ..ProductPatch::default()
            },
        )
        .is_err();
    let exact = assert_reference(
        &catalog,
        FilterQuery {
            exact_path: Some("specs.connector".into()),
            exact_value: Some("usb-c".into()),
            ..FilterQuery::default()
        },
    )?;
    let range = assert_reference(
        &catalog,
        FilterQuery {
            range_path: Some("specs.length_m".into()),
            min: Some(3.0),
            max: Some(4.0),
            ..FilterQuery::default()
        },
    )?;
    println!("demo database: {}", path.display());
    println!("legacy SQLite table preserved: yes");
    println!(
        "v1 readable after v2 activation: spec_version={}",
        before.spec_version
    );
    println!(
        "tested migration: spec_version={}, version={}",
        migrated.spec_version, migrated.version
    );
    println!(
        "partial patch preserved connector={}, new version={}",
        patched.specs["connector"], patched.version
    );
    println!("stale conditional patch rejected: {stale_rejected}");
    println!("independent filter evaluator: exact={exact}, range={range}, all matched");
    println!("Fold decision: no-fit for authoritative catalog path; SQLite retained");
    Ok(())
}

fn run_import_check(total: usize, payload_bytes: usize) -> Result<()> {
    let path = fresh_path(&format!("import-{total}-{payload_bytes}.sqlite"));
    let start = Instant::now();
    {
        let catalog = Catalog::open(&path)?;
        catalog.install_demo_schemas(2)?;
        let interrupted = catalog.resume_import("seed-v1", total, 1_000, Some(7), payload_bytes)?;
        println!(
            "simulated interruption checkpoint: {}/{}",
            interrupted.next_ordinal, total
        );
    }
    let catalog = Catalog::open(&path)?;
    catalog.install_demo_schemas(2)?;
    let resumed = catalog.resume_import("seed-v1", total, 1_000, None, payload_bytes)?;
    let (count, distinct) = catalog.count_products()?;
    catalog.checkpoint()?;
    let bytes = std::fs::metadata(&path).map_err(Failure::internal)?.len();
    println!("resume completed: {}", resumed.completed);
    println!("rows: {count}; distinct ids: {distinct}; expected: {total}");
    println!("record payload: {payload_bytes} bytes; database: {bytes} bytes");
    println!("elapsed: {:.3}s", start.elapsed().as_secs_f64());
    if count != total || distinct != total || !resumed.completed {
        return Err(Failure::internal("import integrity check failed"));
    }
    Ok(())
}

fn run_storage_check(total: usize, payload_bytes: usize) -> Result<()> {
    let indexed_path = fresh_path(&format!("storage-indexed-{total}-{payload_bytes}.sqlite"));
    let baseline_path = fresh_path(&format!("storage-baseline-{total}-{payload_bytes}.sqlite"));
    let catalog = Catalog::open(&indexed_path)?;
    catalog.install_demo_schemas(2)?;
    catalog.resume_import("storage", total, 1_000, None, payload_bytes)?;
    catalog.checkpoint()?;

    let baseline = Connection::open(&baseline_path)?;
    baseline.execute_batch(
        "CREATE TABLE products (
           id TEXT PRIMARY KEY, version INTEGER NOT NULL, category TEXT NOT NULL,
           name TEXT NOT NULL, price_cents INTEGER NOT NULL, description TEXT NOT NULL,
           spec_version INTEGER NOT NULL, specs_json TEXT NOT NULL,
           compatibility_json TEXT NOT NULL, tags_json TEXT NOT NULL
         );
         CREATE INDEX products_category ON products(category);
         CREATE INDEX products_price ON products(price_cents);",
    )?;
    baseline.transaction(|| {
        let mut insert = baseline.prepare(
            "INSERT INTO products
             (id,version,category,name,price_cents,description,spec_version,specs_json,compatibility_json,tags_json)
             VALUES(?,?,?,?,?,?,?,?,?,?)",
        )?;
        for ordinal in 0..total {
            let product = generated_product(ordinal, payload_bytes);
            insert.bind_text(1, &product.id)?;
            insert.bind_i64(2, product.version)?;
            insert.bind_text(3, &product.category)?;
            insert.bind_text(4, &product.name)?;
            insert.bind_i64(5, product.price_cents)?;
            insert.bind_text(6, &product.description)?;
            insert.bind_i64(7, product.spec_version)?;
            insert.bind_text(8, &serde_json::to_string(&product.specs).unwrap())?;
            insert.bind_text(9, &serde_json::to_string(&product.compatibility).unwrap())?;
            insert.bind_text(10, &serde_json::to_string(&product.tags).unwrap())?;
            insert.execute()?;
            insert.reset()?;
        }
        Ok(())
    })?;
    drop(baseline);
    let indexed_bytes = std::fs::metadata(&indexed_path)
        .map_err(Failure::internal)?
        .len();
    let baseline_bytes = std::fs::metadata(&baseline_path)
        .map_err(Failure::internal)?
        .len();
    let ratio = indexed_bytes as f64 / baseline_bytes as f64;
    println!("storage sample: {total} records at {payload_bytes} payload bytes");
    println!("baseline SQLite: {baseline_bytes} bytes");
    println!("indexed SQLite: {indexed_bytes} bytes");
    println!("ratio: {ratio:.3}x; target: <1.5x");
    if ratio >= 1.5 {
        return Err(Failure::internal("storage ratio exceeded 1.5x"));
    }
    Ok(())
}

async fn run_burst(count: usize) -> Result<()> {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    let path = fresh_path(&format!("burst-{count}.sqlite"));
    let catalog = Catalog::open(&path)?;
    catalog.install_demo_schemas(2)?;
    catalog.resume_import("burst-seed", count, 1_000, None, 2_048)?;
    let router = app(catalog);
    let mut tasks = Vec::new();
    for index in 0..150 {
        let service = router.clone();
        tasks.push(tokio::spawn(async move {
            let start = Instant::now();
            let request = if index < 120 {
                Request::builder()
                    .uri(format!("/products/bulk-{:06}", index % count))
                    .body(Body::empty())
                    .unwrap()
            } else {
                Request::builder()
                    .method("PATCH")
                    .header("content-type", "application/json")
                    .uri(format!("/products/bulk-{:06}", index % count))
                    .body(Body::from(format!(
                        r#"{{"expected_version":1,"price_cents":{}}}"#,
                        20_000 + index
                    )))
                    .unwrap()
            };
            let status = service.oneshot(request).await.unwrap().status();
            (index < 120, status, start.elapsed())
        }));
    }
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    for task in tasks {
        let (is_read, status, duration) = task.await.map_err(Failure::internal)?;
        if !status.is_success() {
            return Err(Failure::internal(format!("burst request failed: {status}")));
        }
        if is_read {
            reads.push(duration);
        } else {
            writes.push(duration);
        }
    }
    reads.sort();
    writes.sort();
    let read_p95 = percentile(&reads, 95);
    let write_p95 = percentile(&writes, 95);
    println!("simultaneous burst: 150 requests (120 reads / 30 writes)");
    println!("seed: {count} representative 2 KiB records");
    println!("read p95: {:.3} ms", read_p95.as_secs_f64() * 1_000.0);
    println!("write p95: {:.3} ms", write_p95.as_secs_f64() * 1_000.0);
    println!("targets: reads <50 ms; writes <100 ms");
    Ok(())
}

fn percentile(values: &[Duration], percentile: usize) -> Duration {
    values[((values.len() - 1) * percentile).div_ceil(100)]
}

async fn serve(path: PathBuf) -> Result<()> {
    let catalog = Catalog::open(&path)?;
    catalog.install_demo_schemas(2)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .map_err(Failure::internal)?;
    println!("catalog API listening at http://127.0.0.1:3000");
    axum::serve(listener, app(catalog))
        .await
        .map_err(Failure::internal)
}

#[tokio::main]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = match args.first().map(String::as_str) {
        None | Some("demo") => run_demo(),
        Some("import-check") => {
            let total = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(25_000);
            let bytes = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(2_048);
            run_import_check(total, bytes)
        }
        Some("burst") => {
            let count = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(25_000);
            run_burst(count).await
        }
        Some("storage-check") => {
            let total = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(25_000);
            let bytes = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(2_048);
            run_storage_check(total, bytes)
        }
        Some("serve") => {
            let path = args
                .get(1)
                .map(PathBuf::from)
                .unwrap_or_else(|| fresh_path("server.sqlite"));
            serve(path).await
        }
        Some(command) => Err(Failure::invalid(
            "command",
            format!(
                "unknown command {command}; use demo, import-check, storage-check, burst, or serve"
            ),
        )),
    };
    if let Err(error) = result {
        eprintln!("{}", serde_json::to_string(&error.body).unwrap());
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::collections::BTreeSet;
    use tower::ServiceExt;

    fn catalog(name: &str) -> Catalog {
        let catalog = Catalog::open(&fresh_path(name)).unwrap();
        catalog.install_demo_schemas(2).unwrap();
        catalog
    }

    #[test]
    fn validation_has_stable_nested_path() {
        let catalog = catalog("validation.sqlite");
        let mut input = example_input("bad", "laptop");
        input.specs["battery_wh"] = json!(5);
        let error = catalog.create(input).unwrap_err();
        assert_eq!(error.body.path.as_deref(), Some("specs.battery_wh"));
    }

    #[test]
    fn partial_patch_preserves_omitted_fields_and_rejects_stale_version() {
        let catalog = catalog("patch.sqlite");
        let original = catalog.create(example_input("c-1", "cable")).unwrap();
        let updated = catalog
            .patch(
                "c-1",
                ProductPatch {
                    expected_version: original.version,
                    specs: Some(json!({"length_m":4.0})),
                    ..ProductPatch::default()
                },
            )
            .unwrap();
        assert_eq!(updated.specs["connector"], "usb-c");
        assert_eq!(updated.name, original.name);
        let stale = catalog
            .patch(
                "c-1",
                ProductPatch {
                    expected_version: 1,
                    name: Some("lost update".into()),
                    ..ProductPatch::default()
                },
            )
            .unwrap_err();
        assert_eq!(stale.status, StatusCode::CONFLICT);
    }

    #[test]
    fn old_revision_is_readable_and_migration_is_tested() {
        let path = fresh_path("migration.sqlite");
        let catalog = Catalog::open(&path).unwrap();
        catalog.install_demo_schemas(1).unwrap();
        let mut input = example_input("old", "laptop");
        input.specs = json!({"ram_gb":16,"screen_inches":14.0});
        catalog.create_at_revision(input, 1).unwrap();
        catalog.activate_schema("laptop", 2).unwrap();
        assert_eq!(catalog.get("old").unwrap().spec_version, 1);
        let migrated = catalog.migrate_laptop_v1_to_v2("old", 55).unwrap();
        assert_eq!(migrated.spec_version, 2);
        assert_eq!(migrated.specs["battery_wh"], 55);
    }

    #[test]
    fn exact_and_range_filters_match_independent_evaluator() {
        let catalog = catalog("filters.sqlite");
        catalog
            .resume_import("filters", 300, 100, None, 128)
            .unwrap();
        for query in [
            FilterQuery {
                exact_path: Some("specs.connector".into()),
                exact_value: Some("hdmi".into()),
                ..FilterQuery::default()
            },
            FilterQuery {
                category: Some("laptop".into()),
                range_path: Some("specs.ram_gb".into()),
                min: Some(16.0),
                max: Some(32.0),
                ..FilterQuery::default()
            },
            FilterQuery {
                range_path: Some("specs.max_weight_kg".into()),
                min: Some(100.0),
                max: Some(180.0),
                ..FilterQuery::default()
            },
        ] {
            assert_reference(&catalog, query).unwrap();
        }
    }

    #[test]
    fn interrupted_import_resumes_without_gaps_or_duplicates() {
        let path = fresh_path("resume.sqlite");
        {
            let catalog = Catalog::open(&path).unwrap();
            catalog.install_demo_schemas(2).unwrap();
            let progress = catalog
                .resume_import("job", 2_503, 100, Some(4), 128)
                .unwrap();
            assert_eq!(progress.next_ordinal, 400);
        }
        let catalog = Catalog::open(&path).unwrap();
        catalog.install_demo_schemas(2).unwrap();
        assert!(
            catalog
                .resume_import("job", 2_503, 100, None, 128)
                .unwrap()
                .completed
        );
        assert_eq!(catalog.count_products().unwrap(), (2_503, 2_503));
        let ids = catalog
            .all()
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect::<BTreeSet<_>>();
        assert!((0..2_503).all(|i| ids.contains(&format!("bulk-{i:06}"))));
    }

    #[test]
    fn import_resume_rejects_changed_source() {
        let path = fresh_path("resume-source-mismatch.sqlite");
        {
            let catalog = Catalog::open(&path).unwrap();
            catalog.install_demo_schemas(2).unwrap();
            catalog.resume_import("job", 250, 100, Some(1), 8).unwrap();
        }
        let catalog = Catalog::open(&path).unwrap();
        catalog.install_demo_schemas(2).unwrap();
        let error = catalog
            .resume_import("job", 250, 100, None, 64)
            .unwrap_err();
        assert_eq!(error.body.path.as_deref(), Some("source_fingerprint"));
        assert_eq!(catalog.count_products().unwrap(), (100, 100));
    }

    #[test]
    fn import_rejects_conflicting_preexisting_product() {
        let catalog = catalog("resume-conflict.sqlite");
        catalog
            .create(example_input("bulk-000000", "laptop"))
            .unwrap();

        let error = catalog.resume_import("job", 10, 10, None, 64).unwrap_err();
        assert!(error.body.error.contains("conflicts with existing content"));
        assert!(catalog.import_progress("job").unwrap().is_none());
        assert_eq!(catalog.count_products().unwrap(), (1, 1));
        assert_eq!(catalog.get("bulk-000000").unwrap().name, "Example laptop");
    }

    #[tokio::test]
    async fn http_crud_keeps_product_response_shape() {
        let router = app(catalog("http.sqlite"));
        let input = example_input("api-1", "chair");
        let create = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/products")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&input).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::CREATED);
        let created: Product =
            serde_json::from_slice(&create.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let get = router
            .oneshot(
                Request::builder()
                    .uri("/products/api-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let fetched: Product =
            serde_json::from_slice(&get.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(fetched, created);
    }

    #[test]
    fn opening_catalog_preserves_unrelated_legacy_table() {
        let path = fresh_path("legacy.sqlite");
        let legacy = Connection::open(&path).unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE orders(id INTEGER PRIMARY KEY); INSERT INTO orders VALUES(9);",
            )
            .unwrap();
        drop(legacy);
        let catalog = Catalog::open(&path).unwrap();
        let mut statement = catalog.db.prepare("SELECT id FROM orders").unwrap();
        assert!(matches!(statement.step().unwrap(), Step::Row));
        assert_eq!(statement.column_i64(0), 9);
    }
}
