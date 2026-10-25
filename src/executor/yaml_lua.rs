use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Write;

use mlua::prelude::*;
use postgres::Client;
use rand::distributions::Alphanumeric;
use rand::rngs::SmallRng;
use rand::Rng;
use rand::SeedableRng;

/// Raw pointer wrapper for postgres::Client.
/// SAFETY: This is only used within a single thread. The pointer is set before
/// Lua execution and cleared immediately after. The Lua VM is thread-local.
struct PgClientPtr(*mut Client);

// SAFETY: PgClientPtr is confined to a single thread via thread_local! storage.
// It is never sent across threads or shared between threads.
unsafe impl Send for PgClientPtr {}
unsafe impl Sync for PgClientPtr {}

/// Wrapper for an active COPY writer. Stored in thread-local during copy_start/copy_end.
struct CopyWriterHolder(Option<postgres::CopyInWriter<'static>>);

// SAFETY: Same as PgClientPtr -- single-threaded, never shared.
unsafe impl Send for CopyWriterHolder {}
unsafe impl Sync for CopyWriterHolder {}

thread_local! {
    /// Thread-local Lua VM for script-mode transactions (has DB bridge functions).
    static LUA_VM: RefCell<Option<Lua>> = const { RefCell::new(None) };

    /// Thread-local Lua VM for simple-mode binding evaluation (lightweight, no DB functions).
    static EVAL_VM: RefCell<Option<Lua>> = const { RefCell::new(None) };

    /// Transaction ids whose binding function has been compiled into EVAL_VM.
    static EVAL_FUNCS: RefCell<HashSet<u16>> = RefCell::new(HashSet::new());

    /// Thread-local pointer to the current postgres client.
    static PG_CLIENT: RefCell<PgClientPtr> = const { RefCell::new(PgClientPtr(std::ptr::null_mut())) };

    /// Thread-local active COPY writer (only one at a time).
    static COPY_WRITER: RefCell<CopyWriterHolder> = const { RefCell::new(CopyWriterHolder(None)) };

    /// Thread-local non-cryptographic RNG for data generation. Seeded once per
    /// thread from the OS, then reused across the millions of rand_int/rand_float
    /// calls a load script makes. Avoids rand::thread_rng()'s per-call handle
    /// clone + reseed check and its crypto-grade ChaCha generator, neither of
    /// which we need for synthetic benchmark data.
    static FAST_RNG: RefCell<SmallRng> = RefCell::new(SmallRng::from_entropy());
}

/// Run a closure with a mutable borrow of the thread-local fast RNG.
fn with_rng<F, R>(f: F) -> R
where
    F: FnOnce(&mut SmallRng) -> R,
{
    FAST_RNG.with(|cell| f(&mut cell.borrow_mut()))
}

/// Set the postgres client pointer for the current thread.
/// Must be called before executing any Lua script.
pub fn set_client(client: &mut Client) {
    PG_CLIENT.with(|cell| {
        cell.borrow_mut().0 = client as *mut Client;
    });
}

/// Clear the postgres client pointer. Call after Lua script execution.
pub fn clear_client() {
    PG_CLIENT.with(|cell| {
        cell.borrow_mut().0 = std::ptr::null_mut();
    });
}

/// Get a mutable reference to the postgres client from the thread-local pointer.
/// SAFETY: The pointer was set via `set_client` and is valid for the duration of
/// the Lua script execution. This is only called from within registered Lua functions,
/// which run synchronously on the same thread.
fn with_client<F, R>(f: F) -> LuaResult<R>
where
    F: FnOnce(&mut Client) -> LuaResult<R>,
{
    PG_CLIENT.with(|cell| {
        let ptr = cell.borrow().0;
        if ptr.is_null() {
            return Err(LuaError::RuntimeError(
                "No database connection available".to_string(),
            ));
        }
        // SAFETY: pointer is valid, single-threaded access, set before Lua execution
        let client = unsafe { &mut *ptr };
        f(client)
    })
}

/// Create and configure a new Lua VM with all registered functions.
pub fn create_lua_vm() -> LuaResult<Lua> {
    let lua = Lua::new();

    // rand_int(min, max) and rand_float(min, max)
    register_rand_functions(&lua)?;

    // Register begin()
    let begin = lua.create_function(|_, ()| {
        with_client(|client| {
            client
                .batch_execute("BEGIN")
                .map_err(|e| db_err("begin", &e))
        })
    })?;
    lua.globals().set("begin", begin)?;

    // Register commit()
    let commit = lua.create_function(|_, ()| {
        with_client(|client| {
            client
                .batch_execute("COMMIT")
                .map_err(|e| db_err("commit", &e))
        })
    })?;
    lua.globals().set("commit", commit)?;

    // Register rollback()
    let rollback = lua.create_function(|_, ()| {
        with_client(|client| {
            client
                .batch_execute("ROLLBACK")
                .map_err(|e| db_err("rollback", &e))
        })
    })?;
    lua.globals().set("rollback", rollback)?;

    // Register exec(sql, ...) -- execute a statement, no return value
    let exec_fn = lua.create_function(|_, args: LuaMultiValue| {
        let mut args_iter = args.into_iter();
        let sql: String = match args_iter.next() {
            Some(LuaValue::String(s)) => s.to_str()?.to_string(),
            _ => {
                return Err(LuaError::RuntimeError(
                    "exec: first argument must be a SQL string".to_string(),
                ))
            }
        };

        let params = collect_params_as_literals(args_iter)?;

        with_client(|client| execute_simple(client, &sql, &params).map_err(|e| db_err("exec", &e)))
    })?;
    lua.globals().set("exec", exec_fn)?;

    // Register query(sql, ...) -> array of tables
    let query_fn = lua.create_function(|lua, args: LuaMultiValue| {
        let mut args_iter = args.into_iter();
        let sql: String = match args_iter.next() {
            Some(LuaValue::String(s)) => s.to_str()?.to_string(),
            _ => {
                return Err(LuaError::RuntimeError(
                    "query: first argument must be a SQL string".to_string(),
                ))
            }
        };

        let params = collect_params_as_literals(args_iter)?;

        with_client(|client| {
            let rows = query_simple(client, &sql, &params).map_err(|e| db_err("query", &e))?;
            simple_rows_to_lua(lua, &rows)
        })
    })?;
    lua.globals().set("query", query_fn)?;

    // Register query_one(sql, ...) -> first column of first row
    let query_one_fn = lua.create_function(|lua, args: LuaMultiValue| {
        let mut args_iter = args.into_iter();
        let sql: String = match args_iter.next() {
            Some(LuaValue::String(s)) => s.to_str()?.to_string(),
            _ => {
                return Err(LuaError::RuntimeError(
                    "query_one: first argument must be a SQL string".to_string(),
                ))
            }
        };

        let params = collect_params_as_literals(args_iter)?;

        with_client(|client| {
            let rows = query_simple(client, &sql, &params).map_err(|e| db_err("query", &e))?;

            if rows.is_empty() {
                return Ok(LuaValue::Nil);
            }

            // SimpleQueryRow returns all values as Option<&str>
            // Try to parse as number, otherwise return as string
            match rows[0].get(0) {
                Some(val) => str_to_lua_value(lua, val),
                None => Ok(LuaValue::Nil),
            }
        })
    })?;
    lua.globals().set("query_one", query_one_fn)?;

    // Register copy_start(sql) -- begin a COPY FROM stdin stream
    let copy_start_fn = lua.create_function(|_, sql: LuaString| {
        let sql_str = sql.to_str()?.to_string();
        with_client(|client| {
            let writer = client
                .copy_in(&sql_str)
                .map_err(|e| db_err("copy_start", &e))?;
            // SAFETY: The writer borrows client, but we know client lives for the
            // duration of the Lua script execution. We transmute the lifetime to 'static
            // and store in thread-local. The writer is finished (copy_end) or dropped
            // before the client pointer is cleared.
            let writer: postgres::CopyInWriter<'static> = unsafe { std::mem::transmute(writer) };
            COPY_WRITER.with(|cell| {
                cell.borrow_mut().0 = Some(writer);
            });
            Ok(())
        })
    })?;
    lua.globals().set("copy_start", copy_start_fn)?;

    // Register copy_write(data) -- write a row/string to the active COPY stream
    let copy_write_fn = lua.create_function(|_, data: LuaString| {
        let bytes = data.as_bytes().to_vec();
        COPY_WRITER.with(|cell| {
            let mut holder = cell.borrow_mut();
            match holder.0.as_mut() {
                Some(writer) => {
                    writer
                        .write_all(&bytes)
                        .map_err(|e| db_err("copy_write", &e))?;
                    Ok(())
                }
                None => Err(LuaError::RuntimeError(
                    "copy_write: no active COPY stream (call copy_start first)".to_string(),
                )),
            }
        })
    })?;
    lua.globals().set("copy_write", copy_write_fn)?;

    // Register copy_end() -- finish the COPY stream
    let copy_end_fn = lua.create_function(|_, ()| {
        COPY_WRITER.with(|cell| {
            let mut holder = cell.borrow_mut();
            match holder.0.take() {
                Some(writer) => {
                    writer.finish().map_err(|e| db_err("copy_end", &e))?;
                    Ok(())
                }
                None => Err(LuaError::RuntimeError(
                    "copy_end: no active COPY stream".to_string(),
                )),
            }
        })
    })?;
    lua.globals().set("copy_end", copy_end_fn)?;

    // Register rand_string(min_len, max_len) -- random alphanumeric string
    let rand_string_fn = lua.create_function(|_, (min_len, max_len): (usize, usize)| {
        let len = if min_len < max_len {
            rand::thread_rng().gen_range(min_len..=max_len)
        } else {
            max_len
        };
        let s: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(len)
            .map(char::from)
            .collect();
        Ok(s)
    })?;
    lua.globals().set("rand_string", rand_string_fn)?;

    // Register rand_strings(count, min_len, max_len) -> table of random strings
    // Batched version to reduce Lua↔Rust boundary crossings for bulk data loading.
    let rand_strings_fn =
        lua.create_function(|lua, (count, min_len, max_len): (usize, usize, usize)| {
            let table = lua.create_table_with_capacity(count, 0)?;
            with_rng(|rng| {
                for i in 1..=count {
                    let len = if min_len < max_len {
                        rng.gen_range(min_len..=max_len)
                    } else {
                        max_len
                    };
                    let s: String = (&mut *rng)
                        .sample_iter(&Alphanumeric)
                        .take(len)
                        .map(char::from)
                        .collect();
                    table.set(i, s)?;
                }
                Ok(table)
            })
        })?;
    lua.globals().set("rand_strings", rand_strings_fn)?;

    Ok(lua)
}

/// Inject context variables (min_id, max_id, scalefactor, constants) into the Lua VM.
pub fn inject_context(
    lua: &Lua,
    min_id: u32,
    max_id: u32,
    scalefactor: u32,
    constants: &HashMap<String, serde_yaml::Value>,
) -> LuaResult<()> {
    lua.globals().set("min_id", min_id)?;
    lua.globals().set("max_id", max_id)?;
    lua.globals().set("scalefactor", scalefactor)?;

    let const_table = lua.create_table()?;
    for (key, value) in constants {
        match value {
            serde_yaml::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    const_table.set(key.as_str(), i)?;
                } else if let Some(f) = n.as_f64() {
                    const_table.set(key.as_str(), f)?;
                }
            }
            serde_yaml::Value::String(s) => {
                const_table.set(key.as_str(), s.as_str())?;
            }
            serde_yaml::Value::Bool(b) => {
                const_table.set(key.as_str(), *b)?;
            }
            _ => {}
        }
    }
    lua.globals().set("const", const_table)?;

    Ok(())
}

/// Execute a Lua script on the current thread's VM, using the given client.
/// Creates the VM lazily if it doesn't exist yet.
// TODO(RELEASE_REVIEW D3): bundle the scaling context into a struct.
#[allow(clippy::too_many_arguments)]
pub fn execute_script(
    client: &mut Client,
    script: &str,
    min_id: u32,
    max_id: u32,
    scalefactor: u32,
    constants: &HashMap<String, serde_yaml::Value>,
    scripts: &HashMap<u16, String>,
    modules: &[String],
    tx_id: u16,
) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
    set_client(client);

    let result = LUA_VM.with(|cell| {
        let mut vm_opt = cell.borrow_mut();

        // Lazily initialize the Lua VM
        if vm_opt.is_none() {
            let lua = create_lua_vm().map_err(lua_to_sync_err)?;

            inject_context(&lua, min_id, max_id, scalefactor, constants)
                .map_err(lua_to_sync_err)?;

            // Pre-load shared modules (runs at global scope before any transactions)
            for (i, module_src) in modules.iter().enumerate() {
                lua.load(module_src).exec().map_err(
                    |e| -> Box<dyn std::error::Error + Send + Sync> {
                        Box::new(std::io::Error::other(format!(
                            "Failed to load module {}: {}",
                            i, e
                        )))
                    },
                )?;
            }

            // Pre-load all transaction scripts as named functions
            for (id, script_src) in scripts {
                let func_name = format!("__tx_{}", id);
                let chunk = format!("function {}()\n{}\nend", func_name, script_src);
                lua.load(&chunk).exec().map_err(
                    |e| -> Box<dyn std::error::Error + Send + Sync> {
                        Box::new(std::io::Error::other(format!(
                            "Failed to load script for tx {}: {}",
                            id, e
                        )))
                    },
                )?;
            }

            *vm_opt = Some(lua);
        }

        let lua = vm_opt.as_ref().unwrap();

        // Execute the transaction script
        let start = std::time::Instant::now();

        let func_name = format!("__tx_{}", tx_id);

        // Check if we have a preloaded function
        let globals = lua.globals();
        let func: LuaResult<LuaFunction> = globals.get(&*func_name);

        let outcome: LuaResult<()> = match func {
            Ok(f) => f.call::<()>(()),
            // Fallback: execute inline script directly
            Err(_) => lua.load(script).exec(),
        };
        // A script that errors between copy_start and copy_end leaves the
        // connection in COPY mode; settle the stream before anything else
        // touches the connection (the ROLLBACK issued on error, the next
        // transaction).
        settle_copy_stream(outcome.is_ok());
        outcome.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
            Box::new(std::io::Error::other(e.to_string()))
        })?;

        Ok(start.elapsed().as_micros())
    });

    clear_client();

    result
}

// ---------------------------------------------------------------------------
/// Execute a Lua data-loading script with database access (COPY, exec, query).
/// Creates a fresh Lua VM per call (data loading is infrequent).
pub fn execute_load_script(
    client: &mut Client,
    script: &str,
    scalefactor: u32,
    constants: &HashMap<String, serde_yaml::Value>,
    ids: &[u32],
) -> Result<u128, String> {
    set_client(client);

    let result = (|| -> Result<u128, String> {
        let lua = create_lua_vm().map_err(|e| format!("Failed to create Lua VM: {}", e))?;

        lua.globals()
            .set("scalefactor", scalefactor)
            .map_err(|e| e.to_string())?;
        inject_constants(&lua, constants).map_err(|e| e.to_string())?;

        // Pass the list of IDs to load as a Lua table
        let ids_table = lua.create_table().map_err(|e| e.to_string())?;
        for (i, &id) in ids.iter().enumerate() {
            ids_table.set(i + 1, id).map_err(|e| e.to_string())?;
        }
        lua.globals()
            .set("ids", ids_table)
            .map_err(|e| e.to_string())?;

        let start = std::time::Instant::now();
        let outcome = lua
            .load(script)
            .exec()
            .map_err(|e| format!("Load script failed: {}", e));
        settle_copy_stream(outcome.is_ok());
        outcome?;

        Ok(start.elapsed().as_micros())
    })();

    clear_client();
    result
}

// Internal helpers
// ---------------------------------------------------------------------------

/// Collect Lua variadic arguments as SQL literal strings.
fn collect_params_as_literals(args: impl Iterator<Item = LuaValue>) -> LuaResult<Vec<String>> {
    let mut params = Vec::new();
    for arg in args {
        match arg {
            LuaValue::Integer(i) => params.push(i.to_string()),
            LuaValue::Number(f) => params.push(format!("{}", f)),
            LuaValue::String(s) => {
                let s = s.to_str()?.to_string();
                // Escape single quotes for SQL safety
                params.push(format!("'{}'", s.replace('\'', "''")));
            }
            LuaValue::Boolean(b) => params.push(if b {
                "true".to_string()
            } else {
                "false".to_string()
            }),
            LuaValue::Nil => params.push("NULL".to_string()),
            _ => {
                return Err(LuaError::RuntimeError(format!(
                    "Unsupported parameter type: {:?}",
                    arg
                )))
            }
        }
    }
    Ok(params)
}

/// Substitute $1, $2, ... placeholders with literal values in SQL.
/// Uses simple protocol (like pgbench) to avoid type mismatch issues.
fn interpolate_params(sql: &str, params: &[String]) -> String {
    let mut result = sql.to_string();
    // Replace in reverse order so $10 doesn't match before $1
    for (i, val) in params.iter().enumerate().rev() {
        let placeholder = format!("${}", i + 1);
        result = result.replace(&placeholder, val);
    }
    result
}

/// Execute a SQL statement with interpolated parameters (no results).
fn execute_simple(
    client: &mut Client,
    sql: &str,
    params: &[String],
) -> Result<(), postgres::Error> {
    let final_sql = interpolate_params(sql, params);
    client.batch_execute(&final_sql)
}

/// Execute a SQL query with interpolated parameters, returning rows.
/// Uses simple_query to get results via the simple protocol.
fn query_simple(
    client: &mut Client,
    sql: &str,
    params: &[String],
) -> Result<Vec<postgres::SimpleQueryRow>, postgres::Error> {
    let final_sql = interpolate_params(sql, params);
    let messages = client.simple_query(&final_sql)?;
    let mut rows = Vec::new();
    for msg in messages {
        if let postgres::SimpleQueryMessage::Row(row) = msg {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// Parse a string value into the best-matching Lua type.
fn str_to_lua_value(lua: &Lua, val: &str) -> LuaResult<LuaValue> {
    // Try integer (i32 for Luau compat, fall through to f64 for larger values)
    if let Ok(i) = val.parse::<i32>() {
        return Ok(LuaValue::Integer(i));
    }
    if let Ok(f) = val.parse::<f64>() {
        return Ok(LuaValue::Number(f));
    }
    if val == "t" || val == "true" {
        return Ok(LuaValue::Boolean(true));
    }
    if val == "f" || val == "false" {
        return Ok(LuaValue::Boolean(false));
    }
    Ok(LuaValue::String(lua.create_string(val)?))
}

/// Convert simple query rows to a Lua table of tables.
/// SimpleQueryRow returns all values as Option<&str>, so we auto-detect types.
fn simple_rows_to_lua(lua: &Lua, rows: &[postgres::SimpleQueryRow]) -> LuaResult<LuaValue> {
    let result = lua.create_table()?;

    for (row_idx, row) in rows.iter().enumerate() {
        let row_table = lua.create_table()?;

        for col_idx in 0..row.columns().len() {
            let col_name = row.columns()[col_idx].name();

            if let Some(val) = row.get(col_idx) {
                let lua_val = str_to_lua_value(lua, val)?;
                row_table.set(col_name, lua_val)?;
            }
        }

        result.set(row_idx + 1, row_table)?; // Lua arrays are 1-indexed
    }

    Ok(LuaValue::Table(result))
}

/// Create a lightweight Lua VM with only rand_int/rand_float (no DB functions).
fn create_eval_vm() -> LuaResult<Lua> {
    let lua = Lua::new();
    register_rand_functions(&lua)?;
    Ok(lua)
}

/// Create a Lua VM with rand functions and context already injected.
/// Used by data loading where a fresh VM per load spec is acceptable.
pub fn create_eval_vm_with_context(
    min_id: u32,
    max_id: u32,
    scalefactor: u32,
    constants: &HashMap<String, serde_yaml::Value>,
) -> LuaResult<Lua> {
    let lua = Lua::new();
    register_rand_functions(&lua)?;
    lua.globals().set("min_id", min_id)?;
    lua.globals().set("max_id", max_id)?;
    lua.globals().set("scalefactor", scalefactor)?;
    inject_constants(&lua, constants)?;
    Ok(lua)
}

/// Register `rand_int(min, max)` (inclusive) and `rand_float(min, max)`
/// (half-open). The ranges are validated first: `gen_range` panics on an empty
/// range, and a panic inside an mlua callback takes the whole client or loader
/// thread down. An empty range (e.g. `rand_int(1, max_id - 1)` with
/// `max_id == 1`) is a script bug, reported as a Lua error that names the
/// arguments.
fn register_rand_functions(lua: &Lua) -> LuaResult<()> {
    let rand_int = lua.create_function(|_, (min, max): (i64, i64)| {
        if min > max {
            return Err(LuaError::RuntimeError(format!(
                "rand_int: empty range, min ({min}) is greater than max ({max})"
            )));
        }
        Ok(with_rng(|rng| rng.gen_range(min..=max)))
    })?;
    lua.globals().set("rand_int", rand_int)?;

    let rand_float = lua.create_function(|_, (min, max): (f64, f64)| {
        if !min.is_finite() || !max.is_finite() || min >= max {
            return Err(LuaError::RuntimeError(format!(
                "rand_float: invalid range [{min}, {max}), min must be lower than max and both finite"
            )));
        }
        Ok(with_rng(|rng| rng.gen_range(min..max)))
    })?;
    lua.globals().set("rand_float", rand_float)?;
    Ok(())
}

/// Pre-compile a row template into a Lua function.
///
/// Template: `"{{(id - 1) * 100 + i}}\t{{id}}\t0\t\n"`
/// Compiled Lua:
/// ```lua
/// function(id, i)
///   return tostring((id - 1) * 100 + i) .. "\t" .. tostring(id) .. "\t0\t\n"
/// end
/// ```
/// Pre-compile a row template into a batched Lua function.
/// Returns a function(id, i_start, i_end) that generates all rows for the range
/// as a single concatenated string, using table.concat for efficiency.
///
/// Template: `"{{(id - 1) * 100 + i}}\t{{id}}\t0\t\n"`
/// Compiled Lua:
/// ```lua
/// return function(id, i_start, i_end)
///   local t = {}
///   for i = i_start, i_end do
///     t[#t+1] = tostring((id - 1) * 100 + i) .. "\t" .. tostring(id) .. "\t0\t\n"
///   end
///   return table.concat(t)
/// end
/// ```
pub fn compile_batch_row_template(lua: &Lua, template: &str) -> LuaResult<LuaFunction> {
    let row_expr = parse_template_to_lua_expr(template)?;

    let code = format!(
        "return function(id, i_start, i_end)\n\
         local t = {{}}\n\
         for i = i_start, i_end do\n\
         t[#t+1] = {}\n\
         end\n\
         return table.concat(t)\n\
         end",
        row_expr
    );
    lua.load(&code).eval()
}

/// Parse a `{{expr}}` template string into a Lua concatenation expression.
/// E.g. `"{{id}}\t0\t\n"` -> `tostring(id) .. "\t0\t\n"`
fn parse_template_to_lua_expr(template: &str) -> LuaResult<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut chars = template.chars().peekable();
    let mut literal = String::new();

    while let Some(c) = chars.next() {
        if c == '{' && chars.peek() == Some(&'{') {
            chars.next(); // consume second {

            if !literal.is_empty() {
                parts.push(format!("\"{}\"", literal));
                literal.clear();
            }

            let mut expr = String::new();
            loop {
                match chars.next() {
                    Some('}') if chars.peek() == Some(&'}') => {
                        chars.next();
                        break;
                    }
                    Some(ch) => expr.push(ch),
                    None => {
                        return Err(LuaError::RuntimeError(
                            "Unterminated {{ expression in row template".to_string(),
                        ));
                    }
                }
            }
            parts.push(format!("tostring({})", expr.trim()));
        } else {
            match c {
                '\n' => literal.push_str("\\n"),
                '\t' => literal.push_str("\\t"),
                '\r' => literal.push_str("\\r"),
                '\\' => literal.push_str("\\\\"),
                '"' => literal.push_str("\\\""),
                _ => literal.push(c),
            }
        }
    }

    if !literal.is_empty() {
        parts.push(format!("\"{}\"", literal));
    }

    Ok(if parts.len() == 1 {
        parts[0].clone()
    } else {
        parts.join(" .. ")
    })
}

/// Evaluate binding expressions using a thread-local lightweight VM.
/// Binding expressions are compiled into a single Lua function on first call,
/// then only the pre-compiled function is called on subsequent invocations.
pub fn eval_bindings_simple(
    tx_id: u16,
    bindings: &[(String, String)],
    min_id: u32,
    max_id: u32,
    scalefactor: u32,
    constants: &HashMap<String, serde_yaml::Value>,
) -> Result<HashMap<String, String>, Box<dyn std::error::Error + Send + Sync>> {
    // One binding function per transaction id, so distinct simple transactions
    // don't clobber each other's compiled bindings.
    let func_name = format!("__eval_bindings_{}", tx_id);

    EVAL_VM.with(|cell| {
        let mut vm_opt = cell.borrow_mut();

        if vm_opt.is_none() {
            let lua = create_eval_vm().map_err(lua_to_sync_err)?;
            inject_constants(&lua, constants).map_err(lua_to_sync_err)?;
            *vm_opt = Some(lua);
        }

        let lua = vm_opt.as_ref().unwrap();

        // Set context globals (cheap: just integer assignments)
        lua.globals()
            .set("min_id", min_id)
            .map_err(lua_to_sync_err)?;
        lua.globals()
            .set("max_id", max_id)
            .map_err(lua_to_sync_err)?;
        lua.globals()
            .set("scalefactor", scalefactor)
            .map_err(lua_to_sync_err)?;

        // Compile this transaction's binding function once per thread
        EVAL_FUNCS.with(|set_cell| {
            let mut compiled = set_cell.borrow_mut();

            if !compiled.contains(&tx_id) {
                // Build a Lua function that evaluates all bindings and returns them:
                //   function __eval_bindings_1()
                //     local branch_id = rand_int(min_id, max_id)
                //     local account_id = rand_int(...)
                //     ...
                //     return branch_id, account_id, ...
                //   end
                let mut body = String::new();
                for (key, expr) in bindings {
                    body.push_str(&format!("  local {} = {}\n", key, expr));
                }
                let return_keys: Vec<&str> = bindings.iter().map(|(k, _)| k.as_str()).collect();
                body.push_str(&format!("  return {}\n", return_keys.join(", ")));

                let chunk = format!("function {}()\n{}end", func_name, body);
                lua.load(&chunk).exec().map_err(lua_to_sync_err)?;

                compiled.insert(tx_id);
            }

            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        })?;

        // Call the pre-compiled function (single bytecode dispatch, no parsing)
        let func: LuaFunction = lua
            .globals()
            .get(func_name.as_str())
            .map_err(lua_to_sync_err)?;
        let results: LuaMultiValue = func.call(()).map_err(lua_to_sync_err)?;

        // Map returned values back to binding names
        let mut resolved = HashMap::new();
        for (i, (key, _)) in bindings.iter().enumerate() {
            let val = results.get(i).cloned().unwrap_or(LuaValue::Nil);
            let literal = match &val {
                LuaValue::Integer(n) => n.to_string(),
                LuaValue::Number(f) => format!("{}", f),
                LuaValue::String(s) => s.to_string_lossy().to_string(),
                LuaValue::Boolean(b) => b.to_string(),
                _ => "NULL".to_string(),
            };
            resolved.insert(key.clone(), literal);
        }

        Ok(resolved)
    })
}

/// Inject only the constants table into a Lua VM.
fn inject_constants(lua: &Lua, constants: &HashMap<String, serde_yaml::Value>) -> LuaResult<()> {
    let const_table = lua.create_table()?;
    for (key, value) in constants {
        match value {
            serde_yaml::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    const_table.set(key.as_str(), i)?;
                } else if let Some(f) = n.as_f64() {
                    const_table.set(key.as_str(), f)?;
                }
            }
            serde_yaml::Value::String(s) => {
                const_table.set(key.as_str(), s.as_str())?;
            }
            serde_yaml::Value::Bool(b) => {
                const_table.set(key.as_str(), *b)?;
            }
            _ => {}
        }
    }
    lua.globals().set("const", const_table)?;
    Ok(())
}

fn lua_to_sync_err(e: LuaError) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::other(e.to_string()))
}

/// Close a COPY stream a script left open. After a successful script the
/// stream is finished so the rows it wrote are kept (a script may rely on the
/// implicit `copy_end`); after a failed script it is dropped, which aborts the
/// COPY server-side so partial rows are discarded and the connection leaves
/// COPY mode. Either way the thread-local no longer holds a writer that
/// borrows a client pointer about to be cleared.
fn settle_copy_stream(script_succeeded: bool) {
    COPY_WRITER.with(|cell| {
        if let Some(writer) = cell.borrow_mut().0.take() {
            if script_succeeded {
                let _ = writer.finish();
            } else {
                drop(writer);
            }
        }
    });
}

/// Turn a database-bridge failure into a Lua runtime error that carries the
/// full message. `postgres::Error`'s Display is only a category such as
/// "db error"; the server's message (`division by zero`, the constraint name,
/// `DETAIL:` lines) lives in its source chain, which `error_detail` flattens.
fn db_err(context: &str, e: &dyn std::error::Error) -> LuaError {
    LuaError::RuntimeError(format!("{}: {}", context, super::error_detail(e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn interpolate_params_substitutes_in_reverse_order() {
        // $10 must not be clobbered by the $1 replacement.
        let params: Vec<String> = (1..=10).map(|n| format!("v{}", n)).collect();
        let out = interpolate_params("a=$1 j=$10", &params);
        assert_eq!(out, "a=v1 j=v10");
    }

    #[test]
    fn interpolate_params_replaces_all_occurrences() {
        let params = vec!["42".to_string()];
        assert_eq!(interpolate_params("$1 + $1", &params), "42 + 42");
    }

    #[test]
    fn parse_template_concatenates_exprs_and_literals() {
        // Tab and newline escapes are emitted as Lua escape sequences.
        let expr = parse_template_to_lua_expr("{{id}}\t{{ (id - 1) * 100 + i }}\n").unwrap();
        assert_eq!(
            expr,
            r#"tostring(id) .. "\t" .. tostring((id - 1) * 100 + i) .. "\n""#,
        );
    }

    #[test]
    fn parse_template_single_literal_has_no_concat() {
        assert_eq!(parse_template_to_lua_expr("plain").unwrap(), r#""plain""#);
    }

    #[test]
    fn parse_template_rejects_unterminated_expr() {
        assert!(parse_template_to_lua_expr("{{id").is_err());
    }

    #[test]
    fn str_to_lua_value_detects_types() {
        let lua = Lua::new();
        assert!(matches!(str_to_lua_value(&lua, "42").unwrap(), LuaValue::Integer(n) if n == 42));
        assert!(
            matches!(str_to_lua_value(&lua, "3.5").unwrap(), LuaValue::Number(f) if (f - 3.5).abs() < 1e-9)
        );
        assert!(matches!(
            str_to_lua_value(&lua, "t").unwrap(),
            LuaValue::Boolean(true)
        ));
        assert!(matches!(
            str_to_lua_value(&lua, "false").unwrap(),
            LuaValue::Boolean(false)
        ));
        assert!(matches!(
            str_to_lua_value(&lua, "hello").unwrap(),
            LuaValue::String(_)
        ));
    }

    #[test]
    fn collect_params_escapes_and_maps_types() {
        let lua = Lua::new();
        let values = vec![
            LuaValue::Integer(5),
            LuaValue::String(lua.create_string("O'Brien").unwrap()),
            LuaValue::Boolean(true),
            LuaValue::Nil,
        ];
        let params = collect_params_as_literals(values.into_iter()).unwrap();
        assert_eq!(params, vec!["5", "'O''Brien'", "true", "NULL"]);
    }

    #[test]
    fn batch_row_template_generates_all_rows() {
        let lua = Lua::new();
        let f = compile_batch_row_template(&lua, "{{id}}|{{i}}\n").unwrap();
        let out: String = f.call((3i64, 1i64, 3i64)).unwrap();
        assert_eq!(out, "3|1\n3|2\n3|3\n");
    }

    // Regression for RELEASE_REVIEW B10. Needs a live server (skipped unless
    // PGMETER_TEST_DSN is set): a transaction script that fails between
    // copy_start and copy_end must leave the connection usable and must not
    // keep a stale writer around for the next script on this thread.
    #[test]
    fn failed_script_mid_copy_leaves_connection_usable() {
        use postgres::{Client, NoTls};

        let Ok(dsn) = std::env::var("PGMETER_TEST_DSN") else {
            eprintln!("PGMETER_TEST_DSN not set, skipping");
            return;
        };
        let mut client = Client::connect(&dsn, NoTls).expect("connect to PGMETER_TEST_DSN");
        client
            .batch_execute("DROP TABLE IF EXISTS b10_copy; CREATE TABLE b10_copy (v int)")
            .unwrap();
        let constants = HashMap::new();
        let scripts = HashMap::new();
        let run = |client: &mut Client, script: &str| {
            execute_script(client, script, 1, 1, 1, &constants, &scripts, &[], 910)
        };

        // 1. Fails mid-COPY inside a transaction block.
        let err = run(
            &mut client,
            "begin()\ncopy_start('COPY b10_copy FROM STDIN')\ncopy_write('1\\n')\nerror('boom')",
        )
        .unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");

        // 2. The connection must come back (the executor issues this ROLLBACK
        //    on error, see B3) and the partial COPY must not have landed.
        client
            .batch_execute("ROLLBACK")
            .unwrap_or_else(|e| panic!("connection unusable after mid-COPY failure: {e}"));
        let n: i64 = client
            .query_one("SELECT COUNT(*) FROM b10_copy", &[])
            .unwrap()
            .get(0);
        assert_eq!(n, 0, "aborted COPY must not keep partial rows");

        // 3. A later script on the same thread can run its own COPY to the end.
        run(
            &mut client,
            "copy_start('COPY b10_copy FROM STDIN')\ncopy_write('7\\n8\\n')\ncopy_end()",
        )
        .unwrap();
        let n: i64 = client
            .query_one("SELECT COUNT(*) FROM b10_copy", &[])
            .unwrap()
            .get(0);
        assert_eq!(n, 2);

        client.batch_execute("DROP TABLE b10_copy").unwrap();
    }

    #[test]
    fn rand_int_and_float_stay_in_range() {
        let lua = create_eval_vm().unwrap();
        // A degenerate range is deterministic.
        let i: i64 = lua.load("return rand_int(5, 5)").eval().unwrap();
        assert_eq!(i, 5);
        for _ in 0..100 {
            let f: f64 = lua.load("return rand_float(1.0, 2.0)").eval().unwrap();
            assert!((1.0..2.0).contains(&f));
        }
    }

    // Regression for RELEASE_REVIEW B5: an empty range used to panic inside
    // the callback (`gen_range` "cannot sample empty range"), killing the
    // client thread. It must be an ordinary Lua error the script can see.
    #[test]
    fn rand_int_and_float_reject_empty_ranges_without_panicking() {
        for vm in [create_eval_vm().unwrap(), create_lua_vm().unwrap()] {
            let err = vm.load("return rand_int(5, 1)").eval::<i64>().unwrap_err();
            assert!(err.to_string().contains("rand_int: empty range"), "{err}");

            for call in [
                "rand_float(2.0, 1.0)",
                "rand_float(1.0, 1.0)",
                "rand_float(0/0, 1.0)",
            ] {
                let err = vm.load(format!("return {call}")).eval::<f64>().unwrap_err();
                assert!(
                    err.to_string().contains("rand_float: invalid range"),
                    "{call}: {err}"
                );
            }

            // pcall sees it as a regular error, so scripts can handle it.
            let (ok, msg): (bool, String) = vm
                .load("local ok, err = pcall(rand_int, 10, 2); return ok, tostring(err)")
                .eval()
                .unwrap();
            assert!(!ok);
            assert!(msg.contains("rand_int: empty range"), "{msg}");
        }
    }

    #[test]
    fn eval_bindings_resolves_in_declaration_order() {
        // Later bindings may reference earlier ones; values come back as literals.
        let bindings = vec![
            ("a".to_string(), "min_id + 1".to_string()),
            ("b".to_string(), "a * 2".to_string()),
        ];
        let constants: HashMap<String, serde_yaml::Value> = HashMap::new();
        // Unique tx id so this test's compiled function can't clash with others.
        let resolved = eval_bindings_simple(777, &bindings, 10, 20, 1, &constants).unwrap();
        assert_eq!(resolved.get("a").map(String::as_str), Some("11"));
        assert_eq!(resolved.get("b").map(String::as_str), Some("22"));
    }
}
