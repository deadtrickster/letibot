//! The agent plist and the two units, as text. No RunAtLoad and no KeepAlive: the helper runs
//! only because a connection arrived, and exits when idle.

use crate::{ENV, Manager, Socket, Spec};

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn plist(spec: &Spec) -> String {
    let mut o = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    let kv = |o: &mut String, ind: &str, k: &str, v: &str| {
        o.push_str(&format!(
            "{ind}<key>{}</key>\n{ind}<string>{}</string>\n",
            xml(k),
            xml(v)
        ));
    };
    kv(&mut o, "\t", "Label", &spec.label);
    o.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    for a in &spec.program {
        o.push_str(&format!("\t\t<string>{}</string>\n", xml(a)));
    }
    o.push_str("\t</array>\n\t<key>EnvironmentVariables</key>\n\t<dict>\n");
    kv(&mut o, "\t\t", ENV, Manager::Launchd.as_str());
    for (k, v) in &spec.env {
        kv(&mut o, "\t\t", k, v);
    }
    o.push_str("\t</dict>\n\t<key>Sockets</key>\n\t<dict>\n");
    for s in &spec.sockets {
        o.push_str(&format!("\t\t<key>{}</key>\n\t\t<dict>\n", xml(s.name())));
        match s {
            Socket::Tcp { host, port, .. } => {
                kv(&mut o, "\t\t\t", "SockFamily", "IPv4");
                kv(&mut o, "\t\t\t", "SockNodeName", &host.to_string());
                kv(&mut o, "\t\t\t", "SockServiceName", &port.to_string());
                kv(&mut o, "\t\t\t", "SockType", "stream");
            }
            Socket::Unix { path, .. } => {
                kv(&mut o, "\t\t\t", "SockPathName", &path.to_string_lossy());
                // launchd reads this integer as decimal: 384 is 0600.
                o.push_str("\t\t\t<key>SockPathMode</key>\n\t\t\t<integer>384</integer>\n");
            }
        }
        o.push_str("\t\t</dict>\n");
    }
    o.push_str("\t</dict>\n");
    let log = spec.log.to_string_lossy();
    kv(&mut o, "\t", "StandardOutPath", &log);
    kv(&mut o, "\t", "StandardErrorPath", &log);
    o.push_str("</dict>\n</plist>\n");
    o
}

/// One word of a unit line: `%` and `$` doubled, quoted when it holds a space or a quote.
fn unit_word(s: &str) -> String {
    let s = s.replace('%', "%%").replace('$', "$$");
    if s.is_empty() || s.contains([' ', '\t', '"', '\'', '\\']) {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s
    }
}

pub fn socket_unit(spec: &Spec) -> String {
    let mut o = format!(
        "[Unit]\nDescription={} sockets\n\n[Socket]\n",
        spec.description
    );
    for s in &spec.sockets {
        match s {
            Socket::Tcp { host, port, .. } => o.push_str(&format!("ListenStream={host}:{port}\n")),
            Socket::Unix { path, .. } => o.push_str(&format!(
                "ListenStream={}\n",
                path.to_string_lossy().replace('%', "%%")
            )),
        }
    }
    // One unit names all its fds alike, so the helper matches them by order (`intake`).
    o.push_str(&format!(
        "SocketMode=0600\nDirectoryMode=0700\nService={}.service\n\n\
         [Install]\nWantedBy=sockets.target\n",
        spec.unit
    ));
    o
}

pub fn service_unit(spec: &Spec) -> String {
    let argv: Vec<String> = spec.program.iter().map(|a| unit_word(a)).collect();
    let mut o = format!(
        "[Unit]\nDescription={}\nRequires={}.socket\nAfter={}.socket\n\n[Service]\nExecStart={}\n",
        spec.description,
        spec.unit,
        spec.unit,
        argv.join(" ")
    );
    o.push_str(&format!(
        "Environment={}\n",
        unit_word(&format!("{ENV}={}", Manager::Systemd.as_str()))
    ));
    for (k, v) in &spec.env {
        o.push_str(&format!("Environment={}\n", unit_word(&format!("{k}={v}"))));
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn spec() -> Spec {
        Spec {
            label: "io.letibot.browser".into(),
            unit: "letibot-browser".into(),
            description: "letibot browser bridge".into(),
            program: vec!["/opt/lb/letibot".into(), "browser".into()],
            env: vec![("LETIBOT_STORE".into(), "/data/my store.db".into())],
            sockets: vec![
                Socket::Tcp {
                    name: "http".into(),
                    host: [127, 0, 0, 1].into(),
                    port: 8790,
                },
                Socket::Unix {
                    name: "sock".into(),
                    path: PathBuf::from("/run/user/501/letibot/browser.sock"),
                },
            ],
            log: PathBuf::from("/Users/op/Library/Logs/letibot/browser.log"),
        }
    }

    #[test]
    fn the_plist_is_exact() {
        let want = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>io.letibot.browser</string>
	<key>ProgramArguments</key>
	<array>
		<string>/opt/lb/letibot</string>
		<string>browser</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>LETIBOT_ACTIVATION</key>
		<string>launchd</string>
		<key>LETIBOT_STORE</key>
		<string>/data/my store.db</string>
	</dict>
	<key>Sockets</key>
	<dict>
		<key>http</key>
		<dict>
			<key>SockFamily</key>
			<string>IPv4</string>
			<key>SockNodeName</key>
			<string>127.0.0.1</string>
			<key>SockServiceName</key>
			<string>8790</string>
			<key>SockType</key>
			<string>stream</string>
		</dict>
		<key>sock</key>
		<dict>
			<key>SockPathName</key>
			<string>/run/user/501/letibot/browser.sock</string>
			<key>SockPathMode</key>
			<integer>384</integer>
		</dict>
	</dict>
	<key>StandardOutPath</key>
	<string>/Users/op/Library/Logs/letibot/browser.log</string>
	<key>StandardErrorPath</key>
	<string>/Users/op/Library/Logs/letibot/browser.log</string>
</dict>
</plist>
"#;
        assert_eq!(plist(&spec()), want);
    }

    #[test]
    fn the_units_are_exact() {
        assert_eq!(
            socket_unit(&spec()),
            "[Unit]\nDescription=letibot browser bridge sockets\n\n[Socket]\n\
             ListenStream=127.0.0.1:8790\nListenStream=/run/user/501/letibot/browser.sock\n\
             SocketMode=0600\nDirectoryMode=0700\nService=letibot-browser.service\n\n[Install]\nWantedBy=sockets.target\n"
        );
        assert_eq!(
            service_unit(&spec()),
            "[Unit]\nDescription=letibot browser bridge\nRequires=letibot-browser.socket\n\
             After=letibot-browser.socket\n\n[Service]\nExecStart=/opt/lb/letibot browser\n\
             Environment=LETIBOT_ACTIVATION=systemd\n\
             Environment=\"LETIBOT_STORE=/data/my store.db\"\n"
        );
    }

    #[test]
    fn markup_and_unit_specials_are_escaped() {
        let mut s = spec();
        s.program = vec!["/a&b/<x>".into(), "100%".into(), "$HOME".into()];
        let p = plist(&s);
        assert!(p.contains("<string>/a&amp;b/&lt;x&gt;</string>"), "{p}");
        let u = service_unit(&s);
        assert!(u.contains("ExecStart=/a&b/<x> 100%% $$HOME\n"), "{u}");
    }
}
