use std::path::Path;

use kw_store::sshconfig::{expand_tilde, parse_ssh_config};

#[test]
fn parses_concrete_hosts_and_skips_wildcards() {
    let text = "\
Host prod
  HostName 10.0.0.5
  User deploy
  Port 2222
  IdentityFile ~/.ssh/id_prod   # 行尾注释
Host *
  User root
Host jump backup
  HostName jump.example
  User ops
Host no-user
  HostName x.example
";
    let home = Path::new("C:/Users/alice");
    let hosts = parse_ssh_config(text, home);
    assert_eq!(hosts.len(), 2);
    assert_eq!(hosts[0].name, "prod");
    assert_eq!(hosts[0].host, "10.0.0.5");
    assert_eq!(hosts[0].username, "deploy");
    assert_eq!(hosts[0].port, 2222);
    // 与旧版 path.join 一致：分隔符跟随平台
    assert_eq!(
        hosts[0].identity_file,
        home.join(".ssh/id_prod").display().to_string()
    );
    assert_eq!(hosts[1].name, "jump", "多别名只取第一个");
    assert_eq!(hosts[1].host, "jump.example");
}

#[test]
fn host_defaults_to_alias_and_requires_user() {
    let text = "Host only-host\n  User me\nHost needs-user\n  HostName nope\n";
    let hosts = parse_ssh_config(text, Path::new("/home/a"));
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].host, "only-host", "没写 HostName 时用别名");
}

#[test]
fn quoted_values_and_tilde() {
    let home = Path::new("/home/a");
    assert_eq!(expand_tilde("~", home), "/home/a");
    assert_eq!(
        expand_tilde("~/x", home),
        home.join("x").display().to_string()
    );
    assert_eq!(expand_tilde("/abs/x", home), "/abs/x");

    let hosts = parse_ssh_config(
        "Host q\n  HostName \"q.example\"\n  User \"u\"\n",
        Path::new("/h"),
    );
    assert_eq!(hosts[0].host, "q.example");
    assert_eq!(hosts[0].username, "u");
}

#[test]
fn missing_file_returns_empty() {
    let r = kw_store::sshconfig::import_from_ssh_config(Path::new("/definitely/not/here"));
    assert!(r.is_empty());
}
