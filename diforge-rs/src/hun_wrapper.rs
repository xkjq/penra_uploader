use std::ffi::{CString, CStr};
use std::ptr::null_mut;

enum Hunhandle {}

#[link(name = "hunspell")]
extern "C" {
    fn Hunspell_create(affpath: *const i8, dpath: *const i8) -> *mut Hunhandle;
    fn Hunspell_create_key(affpath: *const i8, dpath: *const i8, key: *const i8) -> *mut Hunhandle;
    fn Hunspell_destroy(pHunspell: *mut Hunhandle);

    fn Hunspell_spell(pHunspell: *mut Hunhandle, word: *const i8) -> i32;
    fn Hunspell_suggest(pHunspell: *mut Hunhandle, slst: *mut *mut *mut i8, word: *const i8) -> i32;
    fn Hunspell_add(pHunspell: *mut Hunhandle, word: *const i8) -> i32;
    fn Hunspell_free_list(pHunspell: *mut Hunhandle, slst: *mut *mut *mut i8, n: i32);
}

type CStringList = *mut *mut i8;

pub struct RawHunspell {
    handle: *mut Hunhandle,
}

macro_rules! extract_vec {
    ( $fname:ident, $handle:expr, $( $arg:expr ),* ) => {
        {
            let mut result = Vec::new();
            unsafe {
                let mut list = null_mut();
                let n = $fname($handle, &mut list, $( $arg ),*) as isize;
                if n != 0 {
                    for i in 0..n {
                        let item = CStr::from_ptr(*list.offset(i));
                        result.push(String::from(item.to_str().unwrap()));
                    }
                    Hunspell_free_list($handle, &mut list, n as i32);
                }
            }
            result
        }
    }
}

impl RawHunspell {
    pub fn new(affpath: &str, dicpath: &str) -> RawHunspell {
        let aff = CString::new(affpath).unwrap();
        let dic = CString::new(dicpath).unwrap();
        unsafe {
            RawHunspell { handle: Hunspell_create(aff.as_ptr(), dic.as_ptr()) }
        }
    }

    pub fn new_with_key(affpath: &str, dicpath: &str, key: &str) -> RawHunspell {
        let aff = CString::new(affpath).unwrap();
        let dic = CString::new(dicpath).unwrap();
        let key = CString::new(key).unwrap();
        unsafe {
            RawHunspell { handle: Hunspell_create_key(aff.as_ptr(), dic.as_ptr(), key.as_ptr()) }
        }
    }

    pub fn check(&self, word: &str) -> bool {
        let c = CString::new(word).unwrap();
        unsafe { Hunspell_spell(self.handle, c.as_ptr()) == 1 }
    }

    pub fn suggest(&self, word: &str) -> Vec<String> {
        let c = CString::new(word).unwrap();
        extract_vec!(Hunspell_suggest, self.handle, c.as_ptr())
    }

    pub fn add(&mut self, word: &str) -> i32 {
        let c = CString::new(word).unwrap();
        unsafe { Hunspell_add(self.handle, c.as_ptr()) }
    }
}

impl Drop for RawHunspell {
    fn drop(&mut self) {
        unsafe { Hunspell_destroy(self.handle); }
    }
}
