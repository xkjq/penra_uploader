use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr::null_mut;

enum Hunhandle {}

#[link(name = "hunspell")]
extern "C" {
    fn Hunspell_create(affpath: *const c_char, dpath: *const c_char) -> *mut Hunhandle;
    fn Hunspell_create_key(
        affpath: *const c_char,
        dpath: *const c_char,
        key: *const c_char,
    ) -> *mut Hunhandle;
    fn Hunspell_destroy(pHunspell: *mut Hunhandle);

    fn Hunspell_spell(pHunspell: *mut Hunhandle, word: *const c_char) -> i32;
    fn Hunspell_suggest(
        pHunspell: *mut Hunhandle,
        slst: *mut *mut *mut c_char,
        word: *const c_char,
    ) -> i32;
    fn Hunspell_add(pHunspell: *mut Hunhandle, word: *const c_char) -> i32;
    fn Hunspell_free_list(pHunspell: *mut Hunhandle, slst: *mut *mut *mut c_char, n: i32);
}

pub struct RawHunspell {
    handle: *mut Hunhandle,
}

macro_rules! extract_vec {
    ( $fname:ident, $handle:expr, $( $arg:expr ),* ) => {
        {
            let mut result = Vec::new();
            if !$handle.is_null() {
                unsafe {
                    let mut list: *mut *mut c_char = null_mut();
                    let n = $fname($handle, &mut list, $( $arg ),*);
                    if n > 0 && !list.is_null() {
                        for i in 0..(n as usize) {
                            let item_ptr = *list.add(i);
                            if !item_ptr.is_null() {
                                let item = CStr::from_ptr(item_ptr);
                                result.push(item.to_string_lossy().into_owned());
                            }
                        }
                        Hunspell_free_list($handle, &mut list, n);
                    }
                }
            }
            result
        }
    }
}

impl RawHunspell {
    pub fn try_new(affpath: &str, dicpath: &str) -> Option<RawHunspell> {
        let aff = CString::new(affpath).ok()?;
        let dic = CString::new(dicpath).ok()?;
        unsafe {
            let handle = Hunspell_create(aff.as_ptr(), dic.as_ptr());
            if handle.is_null() {
                None
            } else {
                Some(RawHunspell { handle })
            }
        }
    }

    pub fn new(affpath: &str, dicpath: &str) -> RawHunspell {
        Self::try_new(affpath, dicpath).expect("Hunspell_create returned null or invalid path")
    }

    pub fn try_new_with_key(affpath: &str, dicpath: &str, key: &str) -> Option<RawHunspell> {
        let aff = CString::new(affpath).ok()?;
        let dic = CString::new(dicpath).ok()?;
        let key = CString::new(key).ok()?;
        unsafe {
            let handle = Hunspell_create_key(aff.as_ptr(), dic.as_ptr(), key.as_ptr());
            if handle.is_null() {
                None
            } else {
                Some(RawHunspell { handle })
            }
        }
    }

    pub fn new_with_key(affpath: &str, dicpath: &str, key: &str) -> RawHunspell {
        Self::try_new_with_key(affpath, dicpath, key)
            .expect("Hunspell_create_key returned null or invalid path")
    }

    pub fn check(&self, word: &str) -> bool {
        if self.handle.is_null() {
            return false;
        }
        let Ok(c) = CString::new(word) else {
            return false;
        };
        unsafe { Hunspell_spell(self.handle, c.as_ptr()) == 1 }
    }

    pub fn suggest(&self, word: &str) -> Vec<String> {
        if self.handle.is_null() {
            return Vec::new();
        }
        let Ok(c) = CString::new(word) else {
            return Vec::new();
        };
        extract_vec!(Hunspell_suggest, self.handle, c.as_ptr())
    }

    pub fn add(&mut self, word: &str) -> i32 {
        if self.handle.is_null() {
            return -1;
        }
        let Ok(c) = CString::new(word) else {
            return -1;
        };
        unsafe { Hunspell_add(self.handle, c.as_ptr()) }
    }
}

impl Drop for RawHunspell {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                Hunspell_destroy(self.handle);
            }
            self.handle = null_mut();
        }
    }
}
