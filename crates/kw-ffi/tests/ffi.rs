//! C ABI 冒烟：模拟 Flutter 侧「初始化一次，然后按方法名调用」的用法。

use std::ffi::{c_char, CStr, CString};

use kw_ffi::{kw_call, kw_free_string, kw_init};

unsafe fn take(ptr: *mut c_char) -> serde_json::Value {
    assert!(!ptr.is_null(), "C ABI 返回了空指针");
    let text = CStr::from_ptr(ptr).to_string_lossy().into_owned();
    kw_free_string(ptr);
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("返回的不是 JSON：{text}（{e}）"))
}

#[test]
fn c_abi_init_bootstrap_and_unknown_method() {
    let dir = tempfile::tempdir().unwrap();
    let app_dir = CString::new(dir.path().to_str().unwrap()).unwrap();
    let init = unsafe { take(kw_init(app_dir.as_ptr())) };
    assert_eq!(init["ok"], true);

    let method = CString::new("bootstrap").unwrap();
    let args = CString::new("{}").unwrap();
    let boot = unsafe { take(kw_call(method.as_ptr(), args.as_ptr())) };
    assert_eq!(boot["ok"], true);
    assert_eq!(
        boot["data"]["localKeys"]["dir"].as_str().unwrap(),
        dir.path().join("keys").to_str().unwrap()
    );

    let method = CString::new("doesNotExist").unwrap();
    let out = unsafe { take(kw_call(method.as_ptr(), args.as_ptr())) };
    assert_eq!(out["ok"], false);
    assert!(out["error"].as_str().unwrap().contains("未知方法"));
}
