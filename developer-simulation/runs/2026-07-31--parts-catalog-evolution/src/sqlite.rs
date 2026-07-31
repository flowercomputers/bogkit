//! Minimal, parameterized SQLite wrapper for the prototype.
//!
//! This avoids adding a database dependency that was not available in the
//! sanitized checkout while still exercising the real system SQLite library.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::ptr::{null, null_mut};

#[allow(non_camel_case_types)]
enum sqlite3 {}
#[allow(non_camel_case_types)]
enum sqlite3_stmt {}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;
const SQLITE_OPEN_FULLMUTEX: c_int = 0x0001_0000;

#[link(name = "sqlite3")]
unsafe extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        pp_db: *mut *mut sqlite3,
        flags: c_int,
        z_vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close_v2(db: *mut sqlite3) -> c_int;
    fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;
    fn sqlite3_exec(
        db: *mut sqlite3,
        sql: *const c_char,
        callback: Option<unsafe extern "C" fn() -> c_int>,
        arg: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut sqlite3,
        sql: *const c_char,
        n_byte: c_int,
        statement: *mut *mut sqlite3_stmt,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_finalize(statement: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_step(statement: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_reset(statement: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_clear_bindings(statement: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_bind_int64(statement: *mut sqlite3_stmt, index: c_int, value: i64) -> c_int;
    fn sqlite3_bind_double(statement: *mut sqlite3_stmt, index: c_int, value: f64) -> c_int;
    fn sqlite3_bind_text(
        statement: *mut sqlite3_stmt,
        index: c_int,
        value: *const c_char,
        length: c_int,
        destructor: unsafe extern "C" fn(*mut c_void),
    ) -> c_int;
    fn sqlite3_bind_null(statement: *mut sqlite3_stmt, index: c_int) -> c_int;
    fn sqlite3_column_int64(statement: *mut sqlite3_stmt, column: c_int) -> i64;
    fn sqlite3_column_text(statement: *mut sqlite3_stmt, column: c_int) -> *const u8;
    fn sqlite3_column_bytes(statement: *mut sqlite3_stmt, column: c_int) -> c_int;
    fn sqlite3_changes64(db: *mut sqlite3) -> i64;
}

unsafe extern "C" fn transient_destructor(_: *mut c_void) {}

#[derive(Debug, Clone)]
pub struct Error(pub String);

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Connection {
    raw: *mut sqlite3,
}

// The connection is opened in SQLite's full-mutex mode and the application
// additionally places it behind a Rust Mutex before sharing it.
unsafe impl Send for Connection {}

impl Connection {
    pub fn open(path: &Path) -> Result<Self> {
        let filename = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| Error("database path contains a NUL byte".into()))?;
        let mut raw = null_mut();
        // SAFETY: filename is NUL-terminated, raw is a valid out pointer, and
        // the returned handle is owned by Connection.
        let rc = unsafe {
            sqlite3_open_v2(
                filename.as_ptr(),
                &mut raw,
                SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX,
                null(),
            )
        };
        if rc != SQLITE_OK {
            let message = if raw.is_null() {
                format!("sqlite open failed with code {rc}")
            } else {
                error_message(raw)
            };
            if !raw.is_null() {
                // SAFETY: raw came from sqlite3_open_v2 and is not retained.
                unsafe { sqlite3_close_v2(raw) };
            }
            return Err(Error(message));
        }
        Ok(Self { raw })
    }

    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        let sql = CString::new(sql).map_err(|_| Error("SQL contains a NUL byte".into()))?;
        // SAFETY: self.raw is an open handle and sql is NUL-terminated.
        let rc = unsafe { sqlite3_exec(self.raw, sql.as_ptr(), None, null_mut(), null_mut()) };
        self.check(rc)
    }

    pub fn prepare(&self, sql: &str) -> Result<Statement> {
        let sql = CString::new(sql).map_err(|_| Error("SQL contains a NUL byte".into()))?;
        let mut raw = null_mut();
        // SAFETY: pointers are valid for this call; SQLite owns the compiled
        // statement until Statement::drop finalizes it.
        let rc = unsafe { sqlite3_prepare_v2(self.raw, sql.as_ptr(), -1, &mut raw, null_mut()) };
        self.check(rc)?;
        Ok(Statement { raw, db: self.raw })
    }

    pub fn changes(&self) -> i64 {
        // SAFETY: self.raw is an open handle.
        unsafe { sqlite3_changes64(self.raw) }
    }

    pub fn transaction<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        self.execute_batch("BEGIN IMMEDIATE")?;
        match operation() {
            Ok(value) => {
                self.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn check(&self, rc: c_int) -> Result<()> {
        if rc == SQLITE_OK {
            Ok(())
        } else {
            Err(Error(error_message(self.raw)))
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: raw is uniquely owned and not used after drop.
        unsafe { sqlite3_close_v2(self.raw) };
    }
}

pub enum Step {
    Row,
    Done,
}

pub struct Statement {
    raw: *mut sqlite3_stmt,
    db: *mut sqlite3,
}

impl Statement {
    pub fn bind_text(&mut self, index: i32, value: &str) -> Result<()> {
        let bytes = value.as_bytes();
        let length =
            i32::try_from(bytes.len()).map_err(|_| Error("bound text too large".into()))?;
        // SAFETY: bytes remains valid for the call and SQLITE_TRANSIENT is
        // represented by the special destructor value documented by SQLite.
        let rc = unsafe {
            sqlite3_bind_text(
                self.raw,
                index,
                bytes.as_ptr().cast(),
                length,
                std::mem::transmute::<*const c_void, unsafe extern "C" fn(*mut c_void)>(
                    (-1_isize) as *const c_void,
                ),
            )
        };
        self.check(rc)
    }

    pub fn bind_i64(&mut self, index: i32, value: i64) -> Result<()> {
        // SAFETY: raw is a valid prepared statement.
        self.check(unsafe { sqlite3_bind_int64(self.raw, index, value) })
    }

    pub fn bind_f64(&mut self, index: i32, value: f64) -> Result<()> {
        // SAFETY: raw is a valid prepared statement.
        self.check(unsafe { sqlite3_bind_double(self.raw, index, value) })
    }

    #[allow(dead_code)]
    pub fn bind_null(&mut self, index: i32) -> Result<()> {
        // SAFETY: raw is a valid prepared statement.
        self.check(unsafe { sqlite3_bind_null(self.raw, index) })
    }

    pub fn step(&mut self) -> Result<Step> {
        // SAFETY: raw is a valid prepared statement.
        match unsafe { sqlite3_step(self.raw) } {
            SQLITE_ROW => Ok(Step::Row),
            SQLITE_DONE => Ok(Step::Done),
            _ => Err(Error(error_message(self.db))),
        }
    }

    pub fn execute(&mut self) -> Result<()> {
        match self.step()? {
            Step::Done => Ok(()),
            Step::Row => Err(Error("statement unexpectedly returned a row".into())),
        }
    }

    #[allow(dead_code)]
    pub fn reset(&mut self) -> Result<()> {
        // SAFETY: raw is a valid prepared statement.
        self.check(unsafe { sqlite3_reset(self.raw) })?;
        // SAFETY: raw is a valid prepared statement.
        self.check(unsafe { sqlite3_clear_bindings(self.raw) })
    }

    pub fn column_i64(&self, column: i32) -> i64 {
        // SAFETY: caller only reads columns while positioned on a row.
        unsafe { sqlite3_column_int64(self.raw, column) }
    }

    pub fn column_text(&self, column: i32) -> String {
        // SAFETY: caller only reads columns while positioned on a row; SQLite
        // owns this buffer until the statement advances or is finalized.
        unsafe {
            let pointer = sqlite3_column_text(self.raw, column);
            if pointer.is_null() {
                return String::new();
            }
            let length = sqlite3_column_bytes(self.raw, column) as usize;
            String::from_utf8_lossy(std::slice::from_raw_parts(pointer, length)).into_owned()
        }
    }

    fn check(&self, rc: c_int) -> Result<()> {
        if rc == SQLITE_OK {
            Ok(())
        } else {
            Err(Error(error_message(self.db)))
        }
    }
}

impl Drop for Statement {
    fn drop(&mut self) {
        // SAFETY: raw is uniquely owned and no longer used after drop.
        unsafe { sqlite3_finalize(self.raw) };
    }
}

fn error_message(db: *mut sqlite3) -> String {
    // SAFETY: db is an open SQLite handle and sqlite3_errmsg returns a stable
    // NUL-terminated string owned by SQLite.
    unsafe {
        CStr::from_ptr(sqlite3_errmsg(db))
            .to_string_lossy()
            .into_owned()
    }
}

#[allow(dead_code)]
fn _keep_transient_symbol_referenced() {
    let _ = transient_destructor as unsafe extern "C" fn(*mut c_void);
}
