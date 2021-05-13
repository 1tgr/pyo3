// Copyright (c) 2017-present PyO3 Project and Contributors

//! Utilities for a Python callable object that invokes a Rust function.

use crate::err::{PyErr, PyResult};
use crate::exceptions::PyOverflowError;
use crate::ffi::{self, Py_hash_t};
use crate::panic::PanicException;
use crate::{GILPool, IntoPyPointer};
use crate::{IntoPy, PyObject, Python};
use std::any::Any;
use std::os::raw::c_int;
use std::panic::{AssertUnwindSafe, UnwindSafe};
use std::{isize, panic};

/// A type which can be the return type of a python C-API callback
pub trait PyCallbackOutput: Copy {
    /// The error value to return to python if the callback raised an exception
    const ERR_VALUE: Self;
}

impl PyCallbackOutput for *mut ffi::PyObject {
    const ERR_VALUE: Self = std::ptr::null_mut();
}

impl PyCallbackOutput for libc::c_int {
    const ERR_VALUE: Self = -1;
}

impl PyCallbackOutput for ffi::Py_ssize_t {
    const ERR_VALUE: Self = -1;
}

impl PyCallbackOutput for () {
    const ERR_VALUE: Self = ();
}

/// Convert the result of callback function into the appropriate return value.
pub trait IntoPyCallbackOutput<Target> {
    fn convert(self, py: Python) -> PyResult<Target>;
}

impl<T, E, U> IntoPyCallbackOutput<U> for Result<T, E>
where
    T: IntoPyCallbackOutput<U>,
    E: Into<PyErr>,
{
    fn convert(self, py: Python) -> PyResult<U> {
        self.map_err(Into::into).and_then(|t| t.convert(py))
    }
}

impl<T> IntoPyCallbackOutput<*mut ffi::PyObject> for T
where
    T: IntoPy<PyObject>,
{
    fn convert(self, py: Python) -> PyResult<*mut ffi::PyObject> {
        Ok(self.into_py(py).into_ptr())
    }
}

impl IntoPyCallbackOutput<Self> for *mut ffi::PyObject {
    fn convert(self, _: Python) -> PyResult<Self> {
        Ok(self)
    }
}

impl IntoPyCallbackOutput<libc::c_int> for () {
    fn convert(self, _: Python) -> PyResult<libc::c_int> {
        Ok(0)
    }
}

impl IntoPyCallbackOutput<libc::c_int> for bool {
    fn convert(self, _: Python) -> PyResult<libc::c_int> {
        Ok(self as c_int)
    }
}

impl IntoPyCallbackOutput<()> for () {
    fn convert(self, _: Python) -> PyResult<()> {
        Ok(())
    }
}

impl IntoPyCallbackOutput<ffi::Py_ssize_t> for usize {
    #[inline]
    fn convert(self, _py: Python) -> PyResult<ffi::Py_ssize_t> {
        if self <= (isize::MAX as usize) {
            Ok(self as isize)
        } else {
            Err(PyOverflowError::new_err(()))
        }
    }
}

// Converters needed for `#[pyproto]` implementations

impl IntoPyCallbackOutput<bool> for bool {
    fn convert(self, _: Python) -> PyResult<bool> {
        Ok(self)
    }
}

impl IntoPyCallbackOutput<usize> for usize {
    fn convert(self, _: Python) -> PyResult<usize> {
        Ok(self)
    }
}

impl<T> IntoPyCallbackOutput<PyObject> for T
where
    T: IntoPy<PyObject>,
{
    fn convert(self, py: Python) -> PyResult<PyObject> {
        Ok(self.into_py(py))
    }
}

pub trait WrappingCastTo<T> {
    fn wrapping_cast(self) -> T;
}

macro_rules! wrapping_cast {
    ($from:ty, $to:ty) => {
        impl WrappingCastTo<$to> for $from {
            #[inline]
            fn wrapping_cast(self) -> $to {
                self as $to
            }
        }
    };
}
wrapping_cast!(u8, Py_hash_t);
wrapping_cast!(u16, Py_hash_t);
wrapping_cast!(u32, Py_hash_t);
wrapping_cast!(usize, Py_hash_t);
wrapping_cast!(u64, Py_hash_t);
wrapping_cast!(i8, Py_hash_t);
wrapping_cast!(i16, Py_hash_t);
wrapping_cast!(i32, Py_hash_t);
wrapping_cast!(isize, Py_hash_t);
wrapping_cast!(i64, Py_hash_t);

pub struct HashCallbackOutput(Py_hash_t);

impl IntoPyCallbackOutput<Py_hash_t> for HashCallbackOutput {
    #[inline]
    fn convert(self, _py: Python) -> PyResult<Py_hash_t> {
        let hash = self.0;
        if hash == -1 {
            Ok(-2)
        } else {
            Ok(hash)
        }
    }
}

impl<T> IntoPyCallbackOutput<HashCallbackOutput> for T
where
    T: WrappingCastTo<Py_hash_t>,
{
    #[inline]
    fn convert(self, _py: Python) -> PyResult<HashCallbackOutput> {
        Ok(HashCallbackOutput(self.wrapping_cast()))
    }
}

#[doc(hidden)]
#[inline]
pub fn convert<T, U>(py: Python, value: T) -> PyResult<U>
where
    T: IntoPyCallbackOutput<U>,
{
    value.convert(py)
}

#[doc(hidden)]
#[inline]
pub fn callback_error<T>() -> T
where
    T: PyCallbackOutput,
{
    T::ERR_VALUE
}

/// Use this macro for all internal callback functions which Python will call.
///
/// It sets up the GILPool and converts the output into a Python object. It also restores
/// any python error returned as an Err variant from the body.
///
/// Finally, any panics inside the callback body will be caught and translated into PanicExceptions.
///
/// # Safety
/// This macro assumes the GIL is held. (It makes use of unsafe code, so usage of it is only
/// possible inside unsafe blocks.)
#[doc(hidden)]
#[macro_export]
macro_rules! callback_body {
    ($py:ident, $body:expr) => {{
        $crate::callback_body_without_convert!($py, $crate::callback::convert($py, $body))
    }};
}

/// Variant of the above which does not perform the callback conversion. This allows the callback
/// conversion to be done manually in the case where lifetimes might otherwise cause issue.
///
/// For example this pyfunction:
///
/// ```ignore
/// fn foo(&self) -> &Bar {
///     &self.bar
/// }
/// ```
///
/// It is wrapped in proc macros with callback_body_without_convert like so:
///
/// ```ignore
/// pyo3::callback_body_without_convert!(py, {
///     let _slf = #slf;
///     pyo3::callback::convert(py, #foo)
/// })
/// ```
///
/// If callback_body was used instead:
///
/// ```ignore
/// pyo3::callback_body!(py, {
///     let _slf = #slf;
///     #foo
/// })
/// ```
///
/// Then this will fail to compile, because the result of #foo borrows _slf, but _slf drops when
/// the block passed to the macro ends.
#[doc(hidden)]
#[macro_export]
macro_rules! callback_body_without_convert {
    ($py:ident, $body:expr) => {{
        $crate::callback::impl_callback_body_without_convert(|$py| $body)
    }};
}

pub fn impl_callback_body_without_convert<F, T>(body: F) -> T
where
    F: FnOnce(Python) -> PyResult<T> + UnwindSafe,
    T: PyCallbackOutput,
{
    let pool = unsafe { GILPool::new() };
    let unwind_safe_py = AssertUnwindSafe(pool.python());
    let panic_result = panic::catch_unwind(move || -> PyResult<_> {
        let py = *unwind_safe_py;
        body(py)
    });

    panic_result_into_callback_output(pool.python(), panic_result)
}

fn panic_result_into_callback_output<T>(
    py: Python,
    panic_result: Result<PyResult<T>, Box<dyn Any + Send + 'static>>,
) -> T
where
    T: PyCallbackOutput,
{
    let py_result = match panic_result {
        Ok(py_result) => py_result,
        Err(panic_err) => Err(PanicException::from_panic(panic_err)),
    };

    py_result.unwrap_or_else(|py_err| {
        py_err.restore(py);
        T::ERR_VALUE
    })
}
