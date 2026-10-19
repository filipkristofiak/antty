//! Shell-command intent only: remote effects are not observed.

use std::path::PathBuf;

use crate::model::{EventDetail, FileEvent, Model, ParticipantId, Scope, TouchKind, TouchSource, Ts};

/// Remote targets, deduplicated by target and access kind in first-seen order.
pub fn targets(script: &str) -> Vec<(String, TouchKind)> {
    let mut found = Vec::new();
    parse_script(script, 0, &mut found);
    found
}

pub(crate) fn push_inferred(model: &mut Model, who: ParticipantId, id: &str, script: &str, start: Ts, end: Ts) {
    for (rel, kind) in targets(script) {
        model.events.push(FileEvent {
            who,
            rel: PathBuf::from(rel),
            scope: Scope::Remote,
            kind,
            source: TouchSource::Inferred,
            start,
            end,
            tool_call_id: Some(id.into()),
            detail: EventDetail::None,
        });
    }
}

fn emit(found: &mut Vec<(String, TouchKind)>, rel: String, kind: TouchKind) {
    if !found.iter().any(|(old, access)| old == &rel && *access == kind) {
        found.push((rel, kind));
    }
}

fn finish_word(
    words: &mut Vec<String>,
    word: &mut String,
    started: &mut bool,
    heredoc_word: &mut bool,
    heredocs: &mut Vec<String>,
    delimiter_next: &mut bool,
) {
    if !*started {
        return;
    }
    *started = false;
    let mut text = std::mem::take(word);
    let is_heredoc = std::mem::take(heredoc_word);
    if *delimiter_next {
        *delimiter_next = false;
        heredocs.push(text);
    } else if is_heredoc {
        let prefix_len = if text.starts_with("<<-") { 3 } else { 2 };
        drop(text.drain(..prefix_len));
        if text.is_empty() {
            *delimiter_next = true;
        } else {
            heredocs.push(text);
        }
    } else {
        words.push(text);
    }
}

fn parse_script(script: &str, depth: usize, found: &mut Vec<(String, TouchKind)>) {
    let mut chars = script.chars().peekable();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut heredoc_word = false;
    let mut quote = None;
    let mut heredocs = Vec::new();
    let mut delimiter_next = false;
    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    word.push(ch);
                }
                continue;
            }
            Some('"') => {
                if ch == '"' {
                    quote = None;
                } else if ch == '\\' {
                    match chars.peek().copied() {
                        Some('"' | '\\' | '$' | '`') => word.push(chars.next().unwrap()),
                        Some('\n') => {
                            chars.next();
                        }
                        _ => word.push(ch),
                    }
                } else {
                    word.push(ch);
                }
                continue;
            }
            _ => {}
        }
        match ch {
            '\'' | '"' => {
                started = true;
                quote = Some(ch);
            }
            '\\' => {
                if let Some(next) = chars.next()
                    && next != '\n'
                {
                    started = true;
                    word.push(next);
                }
            }
            '<' if !started && chars.peek() == Some(&'<') => {
                chars.next();
                started = true;
                heredoc_word = true;
                word.push_str("<<");
            }
            '#' if !started => {
                while chars.peek().is_some_and(|c| *c != '\n') {
                    chars.next();
                }
            }
            '\n' | ';' | '&' | '|' | '(' | ')' | '`' => {
                finish_word(&mut words, &mut word, &mut started, &mut heredoc_word, &mut heredocs, &mut delimiter_next);
                parse_command(&words, depth, found);
                words.clear();
                if ch == '\n' {
                    for delimiter in heredocs.drain(..) {
                        let mut line = String::new();
                        loop {
                            line.clear();
                            let mut ended = false;
                            for next in chars.by_ref() {
                                if next == '\n' {
                                    ended = true;
                                    break;
                                }
                                line.push(next);
                            }
                            if line.trim() == delimiter || !ended {
                                break;
                            }
                        }
                    }
                    delimiter_next = false;
                }
            }
            c if c.is_whitespace() => {
                finish_word(&mut words, &mut word, &mut started, &mut heredoc_word, &mut heredocs, &mut delimiter_next);
            }
            _ => {
                started = true;
                word.push(ch);
            }
        }
    }
    finish_word(&mut words, &mut word, &mut started, &mut heredoc_word, &mut heredocs, &mut delimiter_next);
    parse_command(&words, depth, found);
}

fn assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else { return false };
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn unwrap_prefixes(mut words: &[String]) -> &[String] {
    loop {
        let Some(first) = words.first() else { return words };
        if assignment(first) {
            words = &words[1..];
            continue;
        }
        match basename(first) {
            "nohup" | "time" | "command" | "exec" => words = &words[1..],
            "sudo" | "env" | "timeout" => {
                let wrapper = basename(first);
                words = &words[1..];
                while let Some(flag) = words.first().filter(|s| s.starts_with('-')) {
                    let takes_arg = wrapper == "sudo" && matches!(flag.as_str(), "-u" | "-g");
                    let end_options = flag == "--";
                    words = &words[1..];
                    if takes_arg && !words.is_empty() {
                        words = &words[1..];
                    }
                    if end_options {
                        break;
                    }
                }
                if wrapper == "env" {
                    while words.first().is_some_and(|s| assignment(s)) {
                        words = &words[1..];
                    }
                } else if wrapper == "timeout" && !words.is_empty() {
                    words = &words[1..];
                }
            }
            _ => return words,
        }
    }
}

/// Cursor over options and operands. Stop as soon as a subcommand/target is found,
/// so a remote command's words are never mistaken for local options or targets.
struct Operands<'a> {
    words: &'a [String],
    index: usize,
    options: bool,
}

impl<'a> Operands<'a> {
    fn new(words: &'a [String]) -> Self {
        Self { words, index: 0, options: true }
    }

    fn next(&mut self, short_args: &str, long_args: &[&str]) -> Option<&'a str> {
        while let Some(word) = self.words.get(self.index) {
            self.index += 1;
            let word = word.as_str();
            // Redirections are not copy operands (notably a remote destination
            // followed by `2>&1` or `> log`).
            let redirect = word.trim_start_matches(|c: char| c.is_ascii_digit());
            if redirect.starts_with('<') || redirect.starts_with('>') {
                if redirect.chars().all(|c| matches!(c, '<' | '>')) {
                    self.index += usize::from(self.index < self.words.len());
                }
                continue;
            }
            if self.options && word == "--" {
                self.options = false;
                continue;
            }
            if self.options && word.starts_with('-') && word != "-" {
                if word.starts_with("--") {
                    if !word.contains('=') && long_args.contains(&word) {
                        self.index += usize::from(self.index < self.words.len());
                    }
                } else {
                    for (offset, letter) in word[1..].char_indices() {
                        if short_args.contains(letter) {
                            if offset + letter.len_utf8() == word.len() - 1 {
                                self.index += usize::from(self.index < self.words.len());
                            }
                            break;
                        }
                    }
                }
                continue;
            }
            return Some(word);
        }
        None
    }
}

fn uri_host(authority: &str) -> &str {
    let host = authority.rsplit('@').next().unwrap_or(authority);
    if host.starts_with('[') {
        host.find(']').map(|end| &host[..=end]).unwrap_or(host)
    } else {
        host.split(':').next().unwrap_or(host)
    }
}

fn ssh_host(destination: &str) -> &str {
    if let Some(uri) = destination.strip_prefix("ssh://") {
        uri_host(uri.split('/').next().unwrap_or(uri))
    } else {
        destination.rsplit('@').next().unwrap_or(destination)
    }
}

fn copy_target(operand: &str, rsync: bool) -> Option<String> {
    if rsync && operand.starts_with("rsync://") {
        return None;
    }
    let (host, path) = if let Some(uri) = operand.strip_prefix("scp://") {
        let (authority, path) = match uri.split_once('/') {
            Some((authority, path)) => (authority, Some(path)),
            None => (uri, None),
        };
        let host = uri_host(authority);
        if host.is_empty() {
            return None;
        }
        return Some(match path {
            Some(path) => format!("ssh://{host}/{path}"),
            None => format!("ssh://{host}"),
        });
    } else {
        let (authority, path) = operand.split_once(':')?;
        if authority.is_empty() || authority.contains('/') || (rsync && path.starts_with(':')) {
            return None;
        }
        (authority.rsplit('@').next().unwrap_or(authority), path)
    };
    if host.is_empty() {
        return None;
    }
    Some(if path.is_empty() {
        format!("ssh://{host}")
    } else if path.starts_with('/') {
        format!("ssh://{host}{path}")
    } else {
        format!("ssh://{host}/~/{path}")
    })
}

const RSYNC_LONG_ARGS: &[&str] = &[
    "--rsh",
    "--exclude",
    "--include",
    "--filter",
    "--exclude-from",
    "--include-from",
    "--files-from",
    "--temp-dir",
    "--partial-dir",
    "--link-dest",
    "--compare-dest",
    "--copy-dest",
    "--backup-dir",
    "--suffix",
    "--chmod",
    "--log-file",
    "--password-file",
    "--rsync-path",
];
const COMPOSE_ARGS: &[&str] = &["--file", "--project-name", "--profile", "--env-file"];

fn parse_command(words: &[String], depth: usize, found: &mut Vec<(String, TouchKind)>) {
    let words = unwrap_prefixes(words);
    let Some(program) = words.first() else { return };
    let mut operands = Operands::new(&words[1..]);
    match basename(program) {
        "sh" | "bash" | "zsh" if depth < 2 => {
            if let Some(index) = words.iter().position(|s| matches!(s.as_str(), "-c" | "-lc" | "-ic"))
                && let Some(script) = words.get(index + 1)
            {
                parse_script(script, depth + 1, found);
            }
        }
        "ssh" => {
            if let Some(destination) = operands.next("BbcDEeFIiJLlmOopQRSWw", &[]) {
                let host = ssh_host(destination);
                if !host.is_empty() {
                    emit(found, format!("ssh://{host}"), TouchKind::Read);
                }
            }
        }
        "scp" | "rsync" => {
            let rsync = basename(program) == "rsync";
            let short = if rsync { "efTB" } else { "cFiJloPSX" };
            let long = if rsync { RSYNC_LONG_ARGS } else { &[] };
            // A one-operand command still has a destination; defer emitting each
            // operand until we know whether another follows it.
            let mut previous = operands.next(short, long);
            while let Some(next) = operands.next(short, long) {
                if let Some(rel) = previous.and_then(|s| copy_target(s, rsync)) {
                    emit(found, rel, TouchKind::Read);
                }
                previous = Some(next);
            }
            if let Some(rel) = previous.and_then(|s| copy_target(s, rsync)) {
                emit(found, rel, TouchKind::Write);
            }
        }
        "docker" => match operands.next("Hl", &["--context", "--host", "--config", "--log-level"]) {
            Some("exec") => docker_exec(&mut operands, false, found),
            Some("compose") => compose_exec(&mut operands, found),
            _ => {}
        },
        "docker-compose" => compose_exec(&mut operands, found),
        "kubectl"
            if operands
                .next("ns", &["--namespace", "--context", "--kubeconfig", "--server", "--cluster", "--user"])
                == Some("exec") =>
        {
            if let Some(pod) =
                operands.next("cnf", &["--container", "--namespace", "--filename", "--pod-running-timeout"])
            {
                emit(found, format!("kubectl://{pod}"), TouchKind::Read);
            }
        }
        _ => {}
    }
}

fn compose_exec(operands: &mut Operands<'_>, found: &mut Vec<(String, TouchKind)>) {
    if operands.next("fp", COMPOSE_ARGS) == Some("exec") {
        docker_exec(operands, true, found);
    }
}

fn docker_exec(operands: &mut Operands<'_>, compose: bool, found: &mut Vec<(String, TouchKind)>) {
    let long: &[&str] = if compose {
        &["--env", "--user", "--workdir", "--index"]
    } else {
        &["--env", "--env-file", "--user", "--workdir", "--detach-keys"]
    };
    if let Some(target) = operands.next("euw", long) {
        emit(found, format!("docker://{target}"), TouchKind::Read);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TouchKind::{Read, Write};

    fn check(script: &str, expected: &[(&str, TouchKind)]) {
        let expected: Vec<_> = expected.iter().map(|(rel, kind)| (rel.to_string(), *kind)).collect();
        assert_eq!(targets(script), expected, "script: {script}");
    }

    #[test]
    fn specified_commands() {
        let cases: &[(&str, &[(&str, TouchKind)])] = &[
            ("ssh host -- 'zpool status -v spinners-01'", &[("ssh://host", Read)]),
            ("infocmp -x xterm-ghostty |ssh host -- tic -x -", &[("ssh://host", Read)]),
            ("ssh -i ~/.ssh/k root@host \"docker inspect -f '{{.Name}}' p\"", &[("ssh://host", Read)]),
            ("scp host:/tmp/a.ppm /private/tmp/b.ppm 2>&1", &[("ssh://host/tmp/a.ppm", Read)]),
            ("scp ./x.txt me@box:notes/", &[("ssh://box/~/notes/", Write)]),
            ("rsync -aP -e ssh src/ host:/srv/app/", &[("ssh://host/srv/app/", Write)]),
            ("rsync -a host::mod/x .", &[]),
            ("docker exec -it web sh -c ls", &[("docker://web", Read)]),
            ("docker compose exec -T db psql", &[("docker://db", Read)]),
            ("kubectl -n prod exec -it api-0 -c app -- ls", &[("kubectl://api-0", Read)]),
            ("bash -lc \"ssh h uptime\"", &[("ssh://h", Read)]),
            ("sudo -u x ssh h", &[("ssh://h", Read)]),
            ("uv add --optional ssh paramiko", &[]),
            ("python3 -c 'print(\"ssh key smoke ok\")'", &[]),
            ("uv run --with asyncssh python /tmp/smoke_ssh.py", &[]),
            ("cat <<EOF > f\nssh nothere\nEOF\nssh real", &[("ssh://real", Read)]),
        ];
        for (script, expected) in cases {
            check(script, expected);
        }
    }

    #[test]
    fn quotes_backslashes_and_comments() {
        check(
            "echo 'ssh fake; docker exec fake sh' \"ssh fake | kubectl exec fake\"; # ssh fake\nssh real # docker exec fake",
            &[("ssh://real", Read)],
        );
        check("printf '%s' 'ssh fake'; python -c \"print('ssh fake')\"; 'ssh fake'", &[]);
        check(
            "ssh h\\ ost; ssh 'other host'; ssh \"double\\\"host\"; ssh h\\\n2",
            &[("ssh://h ost", Read), ("ssh://other host", Read), ("ssh://double\"host", Read), ("ssh://h2", Read)],
        );
        check(
            "ssh \"h\\q\"; ssh \"h\\$x\"; ssh host#tag",
            &[("ssh://h\\q", Read), ("ssh://h$x", Read), ("ssh://host#tag", Read)],
        );
        check("echo \"$(ssh fake)\"; echo 'ssh fake'", &[]);
        check("echo '<<EOF'\nssh real", &[("ssh://real", Read)]);
    }

    #[test]
    fn heredoc_forms_and_multiple_delimiters() {
        for opener in ["<<EOF", "<<-EOF", "<<'EOF'", "<<\"EOF\"", "<< EOF", "<<- 'EOF'"] {
            check(&format!("cat {opener}\nssh fake\n\tEOF\nssh real"), &[("ssh://real", Read)]);
        }
        check("cat <<A <<'B' # ssh fake\nssh fake1\nA\ndocker exec fake2 sh\nB\nssh real", &[("ssh://real", Read)]);
        check("cat <<EOF\nssh never\n", &[]);
        check("echo '<<EOF'; ssh real", &[("ssh://real", Read)]);
        check("cat <<EOF\nssh fake\nEOF\ncat << END\nkubectl exec fake\nEND\nssh real", &[("ssh://real", Read)]);
    }

    #[test]
    fn unwraps_repeated_prefixes() {
        check(
            "A=x B='a b' nohup time command exec sudo -n -u user -g group env -i C=y timeout --verbose 5s /usr/bin/ssh host",
            &[("ssh://host", Read)],
        );
        check("env -- A=x sudo -- ssh other; timeout 5 ssh third", &[("ssh://other", Read), ("ssh://third", Read)]);
        check("1A=x ssh fake; =x ssh fake; env; sudo -u; timeout 5", &[]);
    }

    #[test]
    fn ssh_options_and_uris() {
        check(
            "ssh -tt -i~/.ssh/k -p 22 -o ProxyCommand=ssh -v root@h uptime; ssh -- -host; ssh ssh://u@box:2222 uptime",
            &[("ssh://h", Read), ("ssh://-host", Read), ("ssh://box", Read)],
        );
        check(
            "ssh -vi key h; ssh --unknown=value other; ssh ssh://u@[::1]:22",
            &[("ssh://h", Read), ("ssh://other", Read), ("ssh://[::1]", Read)],
        );
        for option in "BbcDEeFIiJLlmOopQRSWw".chars() {
            check(&format!("ssh -{option} ignored real"), &[("ssh://real", Read)]);
        }
        check("ssh -i; ssh; ssh ssh://", &[]);
    }

    #[test]
    fn copy_sources_destinations_and_redirections() {
        check(
            "scp -vP2222 -i key a:/x b:rel c: > log 2>&1",
            &[("ssh://a/x", Read), ("ssh://b/~/rel", Read), ("ssh://c", Write)],
        );
        check(
            "scp -- local h:/x; scp scp://u@h:22/a local; scp local scp://h:22/b/",
            &[("ssh://h/x", Write), ("ssh://h/a", Read), ("ssh://h/b/", Write)],
        );
        check("scp ./a:b /tmp/a:b :empty local; rsync rsync://h/mod local; rsync h::mod local", &[]);
        check("scp h:x h:x", &[("ssh://h/~/x", Read), ("ssh://h/~/x", Write)]);
        check("scp -- -h:x local; scp local h:/x >log", &[("ssh://-h/~/x", Read), ("ssh://h/x", Write)]);
        check(
            "scp scp://h/ local; scp local scp://h; rsync local h:/x::y",
            &[("ssh://h/", Read), ("ssh://h", Write), ("ssh://h/x::y", Write)],
        );
        for option in "cFiJloPSX".chars() {
            check(&format!("scp -{option} ignored local h:x"), &[("ssh://h/~/x", Write)]);
        }
    }

    #[test]
    fn rsync_argument_options() {
        for option in RSYNC_LONG_ARGS {
            check(&format!("rsync {option} ssh local h:/x"), &[("ssh://h/x", Write)]);
            check(&format!("rsync {option}=ssh local h:/x"), &[("ssh://h/x", Write)]);
        }
        for option in "efTB".chars() {
            check(&format!("rsync -a{option} ignored local h:/x"), &[("ssh://h/x", Write)]);
            check(&format!("rsync -a{option}ignored local h:/x"), &[("ssh://h/x", Write)]);
        }
        check("rsync -a --unknown local h:x; rsync -- h:y local", &[("ssh://h/~/x", Write), ("ssh://h/~/y", Read)]);
    }

    #[test]
    fn docker_compose_and_kubectl_options() {
        check(
            "docker --context c -H unix:/socket --config dir -l debug exec -it -e A=x --env-file file -u user -w / --detach-keys=x web ssh fake",
            &[("docker://web", Read)],
        );
        check(
            "docker compose -f compose.yml -p project --profile prod --env-file file exec -T --index 2 -e A=x -u user -w / db sh",
            &[("docker://db", Read)],
        );
        check(
            "docker-compose --file=compose.yml exec --env A=x service sh; docker --host=socket exec -- -container sh",
            &[("docker://service", Read), ("docker://-container", Read)],
        );
        check(
            "kubectl --context c --kubeconfig file -s server --cluster c --user u -n prod exec -it -c app -n prod -f file --pod-running-timeout 1m pod -- ssh fake",
            &[("kubectl://pod", Read)],
        );
        check("kubectl --namespace=prod exec --container=app -- pod sh", &[("kubectl://pod", Read)]);
        check("docker inspect web; docker compose logs db; docker-compose up; kubectl get pods", &[]);
    }

    #[test]
    fn recursion_bounds() {
        check("sh -ic 'ssh first'; bash -lc \"zsh -c 'ssh second'\"", &[("ssh://first", Read), ("ssh://second", Read)]);
        check("bash -c \"sh -c \\\"zsh -c 'ssh too-deep'\\\"\"", &[]);
        check(
            "ssh outer 'ssh inner'; docker exec web sh -c 'ssh inner'; kubectl exec pod -- sh -c 'ssh inner'",
            &[("ssh://outer", Read), ("docker://web", Read), ("kubectl://pod", Read)],
        );
    }

    #[test]
    fn command_boundaries_and_ordered_deduplication() {
        check(
            "ssh a && ssh b || ssh a; (ssh c)\nssh d & ssh e | ssh f; echo `ssh g`; echo $(ssh h)",
            &[
                ("ssh://a", Read),
                ("ssh://b", Read),
                ("ssh://c", Read),
                ("ssh://d", Read),
                ("ssh://e", Read),
                ("ssh://f", Read),
                ("ssh://g", Read),
                ("ssh://h", Read),
            ],
        );
        check(
            "scp h:x local; ssh h; scp local h:x; scp h:x local",
            &[("ssh://h/~/x", Read), ("ssh://h", Read), ("ssh://h/~/x", Write)],
        );
    }
}
