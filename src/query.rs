use std::borrow::Cow;
use std::time::Duration;

use base64::Engine;
use bb8::PooledConnection;
use bb8_tiberius::ConnectionManager;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tiberius::{ColumnData, ColumnType, QueryItem, Row, ToSql};

use crate::error::BridgeError;

#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<Value>,
    #[serde(default)]
    pub database: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct QueryResponse {
    pub result_sets: Vec<ResultSet>,
}

#[derive(Debug, Serialize)]
pub struct ResultSet {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Value>,
}

#[derive(Debug, Serialize)]
pub struct ColumnMeta {
    pub name: String,
    #[serde(rename = "type")]
    pub sql_type: String,
}

#[derive(Debug)]
enum OwnedParam {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl ToSql for OwnedParam {
    fn to_sql(&self) -> ColumnData<'_> {
        match self {
            OwnedParam::Null => ColumnData::I32(None),
            OwnedParam::Bool(b) => ColumnData::Bit(Some(*b)),
            OwnedParam::Int(i) => ColumnData::I64(Some(*i)),
            OwnedParam::Float(f) => ColumnData::F64(Some(*f)),
            OwnedParam::Str(s) => ColumnData::String(Some(Cow::Borrowed(s.as_str()))),
        }
    }
}

fn convert_params(raw: &[Value]) -> Result<Vec<OwnedParam>, BridgeError> {
    raw.iter()
        .enumerate()
        .map(|(i, v)| match v {
            Value::Null => Ok(OwnedParam::Null),
            Value::Bool(b) => Ok(OwnedParam::Bool(*b)),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(OwnedParam::Int(i))
                } else if let Some(f) = n.as_f64() {
                    Ok(OwnedParam::Float(f))
                } else {
                    Err(BridgeError::Internal(format!(
                        "param @P{} number out of range",
                        i + 1
                    )))
                }
            }
            Value::String(s) => Ok(OwnedParam::Str(s.clone())),
            other => Err(BridgeError::Internal(format!(
                "param @P{} unsupported json type: {}",
                i + 1,
                type_name(other)
            ))),
        })
        .collect()
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub async fn stream_execute<F, Fut>(
    pool: std::sync::Arc<bb8::Pool<ConnectionManager>>,
    req: QueryRequest,
    rows_as_objects: bool,
    mut emit: F,
) -> Result<(), BridgeError>
where
    F: FnMut(bytes::Bytes) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    if req.sql.trim().is_empty() {
        return Err(BridgeError::EmptySql);
    }

    let owned = convert_params(&req.params)?;
    let refs: Vec<&dyn ToSql> = owned.iter().map(|p| p as &dyn ToSql).collect();

    let mut conn = pool
        .get()
        .await
        .map_err(|e| BridgeError::Pool(e.to_string()))?;

    let mut stream = conn
        .query(req.sql.as_str(), &refs)
        .await
        .map_err(BridgeError::from_tiberius)?;

    let mut result_set_idx: i64 = -1;
    let mut current_columns: Vec<ColumnMeta> = Vec::new();

    while let Some(item) = stream
        .try_next()
        .await
        .map_err(BridgeError::from_tiberius)?
    {
        match item {
            QueryItem::Metadata(meta) => {
                result_set_idx += 1;
                current_columns = meta
                    .columns()
                    .iter()
                    .map(|c| ColumnMeta {
                        name: c.name().to_string(),
                        sql_type: format!("{:?}", c.column_type()),
                    })
                    .collect();
                let frame = serde_json::json!({
                    "type": "metadata",
                    "result_set": result_set_idx,
                    "columns": &current_columns,
                });
                if !write_frame(&mut emit, frame).await {
                    return Ok(());
                }
            }
            QueryItem::Row(row) => {
                let values = if rows_as_objects {
                    encode_row_object(&row, &current_columns)?
                } else {
                    encode_row_array(&row, &current_columns)?
                };
                let frame = serde_json::json!({
                    "type": "row",
                    "result_set": result_set_idx,
                    "values": values,
                });
                if !write_frame(&mut emit, frame).await {
                    return Ok(());
                }
            }
        }
    }

    let end = serde_json::json!({ "type": "end" });
    write_frame(&mut emit, end).await;
    Ok(())
}

async fn write_frame<F, Fut>(emit: &mut F, value: Value) -> bool
where
    F: FnMut(bytes::Bytes) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let mut buf = serde_json::to_vec(&value).unwrap_or_default();
    buf.push(b'\n');
    emit(bytes::Bytes::from(buf)).await
}

pub async fn execute(
    conn: &mut PooledConnection<'_, ConnectionManager>,
    req: &QueryRequest,
    query_timeout_secs: u64,
    max_rows: usize,
    rows_as_objects: bool,
) -> Result<QueryResponse, BridgeError> {
    if req.sql.trim().is_empty() {
        return Err(BridgeError::EmptySql);
    }

    let owned = convert_params(&req.params)?;
    let refs: Vec<&dyn ToSql> = owned.iter().map(|p| p as &dyn ToSql).collect();

    let fut = async {
        let stream = conn
            .query(req.sql.as_str(), &refs)
            .await
            .map_err(BridgeError::from_tiberius)?;

        collect_result_sets(stream, max_rows, rows_as_objects).await
    };

    match tokio::time::timeout(Duration::from_secs(query_timeout_secs), fut).await {
        Ok(Ok(rs)) => Ok(QueryResponse { result_sets: rs }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(BridgeError::QueryTimeout(query_timeout_secs)),
    }
}

async fn collect_result_sets(
    mut stream: tiberius::QueryStream<'_>,
    max_rows: usize,
    rows_as_objects: bool,
) -> Result<Vec<ResultSet>, BridgeError> {
    let mut out: Vec<ResultSet> = Vec::new();
    let mut total_rows = 0usize;

    while let Some(item) = stream
        .try_next()
        .await
        .map_err(BridgeError::from_tiberius)?
    {
        match item {
            QueryItem::Metadata(meta) => {
                let columns = meta
                    .columns()
                    .iter()
                    .map(|c| ColumnMeta {
                        name: c.name().to_string(),
                        sql_type: format!("{:?}", c.column_type()),
                    })
                    .collect();
                out.push(ResultSet {
                    columns,
                    rows: Vec::new(),
                });
            }
            QueryItem::Row(row) => {
                total_rows += 1;
                if total_rows > max_rows {
                    return Err(BridgeError::ResultTooLarge);
                }
                let current = out
                    .last_mut()
                    .ok_or_else(|| BridgeError::Internal("row before metadata".into()))?;
                let encoded = if rows_as_objects {
                    encode_row_object(&row, &current.columns)?
                } else {
                    encode_row_array(&row, &current.columns)?
                };
                current.rows.push(encoded);
            }
        }
    }

    Ok(out)
}

fn encode_row_array(row: &Row, columns: &[ColumnMeta]) -> Result<Value, BridgeError> {
    let mut arr = Vec::with_capacity(columns.len());
    for (i, _) in columns.iter().enumerate() {
        arr.push(cell_to_json(row, i)?);
    }
    Ok(Value::Array(arr))
}

fn encode_row_object(row: &Row, columns: &[ColumnMeta]) -> Result<Value, BridgeError> {
    let mut obj = Map::with_capacity(columns.len());
    for (i, col) in columns.iter().enumerate() {
        obj.insert(col.name.clone(), cell_to_json(row, i)?);
    }
    Ok(Value::Object(obj))
}

fn cell_to_json(row: &Row, i: usize) -> Result<Value, BridgeError> {
    let col = row
        .columns()
        .get(i)
        .ok_or_else(|| BridgeError::Internal(format!("column {i} missing")))?;
    let ct = col.column_type();

    let v = match ct {
        ColumnType::Null => Value::Null,

        ColumnType::Bit | ColumnType::Bitn => row
            .try_get::<bool, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, Value::Bool),

        ColumnType::Int1 => row
            .try_get::<u8, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |v| Value::Number(v.into())),
        ColumnType::Int2 => row
            .try_get::<i16, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |v| Value::Number(v.into())),
        ColumnType::Int4 => row
            .try_get::<i32, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |v| Value::Number(v.into())),
        ColumnType::Int8 => row
            .try_get::<i64, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |v| Value::Number(v.into())),
        ColumnType::Intn => {
            // width not fixed — try progressively wider integers.
            if let Ok(Some(v)) = row.try_get::<i32, _>(i) {
                Value::Number(v.into())
            } else if let Ok(Some(v)) = row.try_get::<i64, _>(i) {
                Value::Number(v.into())
            } else if let Ok(Some(v)) = row.try_get::<i16, _>(i) {
                Value::Number(v.into())
            } else if let Ok(Some(v)) = row.try_get::<u8, _>(i) {
                Value::Number(v.into())
            } else {
                Value::Null
            }
        }

        ColumnType::Float4 => row
            .try_get::<f32, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .and_then(|v| serde_json::Number::from_f64(v as f64).map(Value::Number))
            .unwrap_or(Value::Null),
        ColumnType::Float8 | ColumnType::Floatn => row
            .try_get::<f64, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
            .unwrap_or(Value::Null),

        ColumnType::Money | ColumnType::Money4 | ColumnType::Numericn | ColumnType::Decimaln => row
            .try_get::<rust_decimal::Decimal, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |d| Value::String(d.to_string())),

        ColumnType::Datetime | ColumnType::Datetimen | ColumnType::Datetime2 => row
            .try_get::<chrono::NaiveDateTime, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |v| {
                Value::String(v.format("%Y-%m-%dT%H:%M:%S%.f").to_string())
            }),
        ColumnType::Datetime4 => row
            .try_get::<chrono::NaiveDateTime, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |v| {
                Value::String(v.format("%Y-%m-%dT%H:%M:%S").to_string())
            }),
        ColumnType::DatetimeOffsetn => row
            .try_get::<chrono::DateTime<chrono::FixedOffset>, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |v| Value::String(v.to_rfc3339())),
        ColumnType::Daten => row
            .try_get::<chrono::NaiveDate, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |v| Value::String(v.to_string())),
        ColumnType::Timen => row
            .try_get::<chrono::NaiveTime, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |v| {
                Value::String(v.format("%H:%M:%S%.f").to_string())
            }),

        ColumnType::Guid => row
            .try_get::<uuid::Uuid, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |v| Value::String(v.to_string())),

        ColumnType::NVarchar
        | ColumnType::NChar
        | ColumnType::BigVarChar
        | ColumnType::BigChar
        | ColumnType::Text
        | ColumnType::NText
        | ColumnType::Xml => row
            .try_get::<&str, _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |s| Value::String(s.to_string())),

        ColumnType::BigVarBin | ColumnType::BigBinary | ColumnType::Image => row
            .try_get::<&[u8], _>(i)
            .map_err(BridgeError::from_tiberius)?
            .map_or(Value::Null, |b| {
                Value::String(base64::engine::general_purpose::STANDARD.encode(b))
            }),

        other => {
            return Err(BridgeError::UnsupportedType(format!("{other:?}")));
        }
    };
    Ok(v)
}
