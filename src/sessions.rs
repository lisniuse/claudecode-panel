use memchr::memmem::Finder;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub cwd: Option<PathBuf>,
    pub modified: SystemTime,
    pub transcripts: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub key: String,
    pub name: String,
    pub path: String,
    pub sessions: Vec<Session>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub projects: Vec<Project>,
    pub error: Option<String>,
}

#[derive(PartialEq, Eq)]
struct Stamp {
    modified: SystemTime,
    size: u64,
}

struct Cached {
    stamp: Stamp,
    session: Session,
}

fn projects_root() -> Option<PathBuf> {
    let config = env::var_os("CLAUDE_CONFIG_DIR").filter(|path| !path.is_empty());
    let root = config.map(PathBuf::from).or_else(|| {
        env::var_os("USERPROFILE").map(|profile| PathBuf::from(profile).join(".claude"))
    })?;
    std::path::absolute(root.join("projects")).ok()
}

pub fn watch() -> (Receiver<Snapshot>, SyncSender<()>) {
    let (sender, receiver) = mpsc::sync_channel(1);
    let (refresh, requests) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut cache = HashMap::new();
        let mut last = None;
        let mut chinese = crate::i18n::chinese();
        let root = projects_root();
        loop {
            let next_language = crate::i18n::chinese();
            if chinese != next_language {
                cache.clear();
                chinese = next_language;
            }
            let snapshot = match &root {
                Some(root) => scan(root, &mut cache).unwrap_or_else(|err| Snapshot {
                    error: Some(trf!(
                        "Could not read sessions: {err}",
                        "读取会话失败：{err}"
                    )),
                    ..Snapshot::default()
                }),
                None => Snapshot {
                    error: Some(
                        tr!(
                            "Could not find the Claude Code configuration directory",
                            "找不到 Claude Code 的配置目录"
                        )
                        .into(),
                    ),
                    ..Snapshot::default()
                },
            };
            if last.as_ref() != Some(&snapshot) {
                if sender.send(snapshot.clone()).is_err() {
                    break;
                }
                last = Some(snapshot);
            }
            if requests.recv_timeout(Duration::from_secs(2))
                == Err(mpsc::RecvTimeoutError::Disconnected)
            {
                break;
            }
        }
    });
    (receiver, refresh)
}

fn scan(root: &Path, cache: &mut HashMap<PathBuf, Cached>) -> io::Result<Snapshot> {
    let directories = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            cache.clear();
            return Ok(Snapshot::default());
        }
        Err(error) => return Err(error),
    };
    let mut previous = std::mem::take(cache);
    let mut projects = BTreeMap::<String, Project>::new();
    for directory in directories.flatten() {
        if !directory.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(entries) = fs::read_dir(directory.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            let Some(id) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|id| valid_id(id))
            else {
                continue;
            };
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let stamp = Stamp {
                modified,
                size: metadata.len(),
            };
            let old = previous.remove(&path);
            let cached = match old {
                Some(old) if old.stamp == stamp => old,
                old => match File::open(&path)
                    .and_then(|file| parse(BufReader::new(file), id, modified, &path))
                {
                    Ok(session) => Cached { stamp, session },
                    Err(_) => match old {
                        Some(old) => old,
                        None => continue,
                    },
                },
            };

            let bucket = directory.file_name();
            let (key, name, display_path) = if let Some(cwd) = &cached.session.cwd {
                let path = cwd.to_string_lossy();
                let name = cwd.file_name().unwrap_or(cwd.as_os_str());
                (
                    path.replace('/', "\\").to_lowercase(),
                    name.to_string_lossy().into_owned(),
                    path.into_owned(),
                )
            } else {
                let bucket = bucket.to_string_lossy();
                (
                    format!("unknown:{bucket}"),
                    bucket.into_owned(),
                    tr!("Unknown working directory", "工作目录未知").into(),
                )
            };
            projects
                .entry(key.clone())
                .or_insert_with(|| Project {
                    key,
                    name,
                    path: display_path,
                    sessions: Vec::new(),
                })
                .sessions
                .push(cached.session.clone());
            cache.insert(path, cached);
        }
    }
    let mut projects: Vec<_> = projects.into_values().collect();
    for project in &mut projects {
        project.sessions.sort_unstable_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| a.id.cmp(&b.id))
                .then_with(|| a.transcripts[0].cmp(&b.transcripts[0]))
        });
        let mut indices = HashMap::<String, usize>::new();
        let mut unique: Vec<Session> = Vec::new();
        for mut session in project.sessions.drain(..) {
            let id = session.id.to_ascii_lowercase();
            if let Some(&index) = indices.get(&id) {
                unique[index].transcripts.append(&mut session.transcripts);
            } else {
                indices.insert(id, unique.len());
                unique.push(session);
            }
        }
        project.sessions = unique;
    }
    projects.sort_unstable_by(|a, b| {
        b.sessions[0]
            .modified
            .cmp(&a.sessions[0].modified)
            .then_with(|| a.key.cmp(&b.key))
    });
    Ok(Snapshot {
        projects,
        error: None,
    })
}

pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn parse(
    mut input: impl BufRead,
    id: &str,
    modified: SystemTime,
    path: &Path,
) -> io::Result<Session> {
    let mut cwd = None;
    let mut custom_title = None;
    let mut generated_title = None;
    let mut first_prompt = None;
    let cwd_field = Finder::new(br#""cwd""#);
    let user_type = Finder::new(br#""user""#);
    let title_fields = [
        br#""customTitle""#.as_slice(),
        br#""aiTitle""#,
        br#""summary""#,
    ]
    .map(Finder::new);
    let mut line = Vec::new();
    while input.read_until(b'\n', &mut line)? != 0 {
        // Assistant output and tool results can be large. Decode only rows that
        // could contribute directory or title metadata to the index.
        let candidate = (cwd.is_none() && cwd_field.find(&line).is_some())
            || (first_prompt.is_none() && user_type.find(&line).is_some())
            || title_fields.iter().any(|field| field.find(&line).is_some());
        if candidate && let Ok(row) = serde_json::from_slice::<Value>(&line) {
            if cwd.is_none() && row["isSidechain"].as_bool() != Some(true) {
                cwd = row["cwd"]
                    .as_str()
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute());
            }
            match row["type"].as_str() {
                Some("custom-title") => custom_title = title_from(row["customTitle"].as_str()),
                Some("ai-title") => {
                    generated_title = title_from(row["aiTitle"].as_str()).or(generated_title);
                }
                Some("summary") if generated_title.is_none() => {
                    generated_title = title_from(row["summary"].as_str());
                }
                Some("user")
                    if first_prompt.is_none()
                        && row["isMeta"].as_bool() != Some(true)
                        && row["isSidechain"].as_bool() != Some(true) =>
                {
                    let content = &row["message"]["content"];
                    first_prompt = prompt_title(content);
                }
                _ => {}
            }
        }
        line.clear();
    }
    Ok(Session {
        id: id.into(),
        title: custom_title
            .or(generated_title)
            .or(first_prompt)
            .unwrap_or_else(|| trf!("Session {}", "会话 {}", &id[..8])),
        cwd,
        modified,
        transcripts: vec![path.to_path_buf()],
    })
}

fn mutation_root() -> io::Result<PathBuf> {
    projects_root()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                tr!("Could not find the sessions directory", "找不到会话目录"),
            )
        })?
        .canonicalize()
}

fn check_record(root: &Path, path: &Path, id: &str) -> io::Result<PathBuf> {
    let name_matches = valid_id(id)
        && path.extension().is_some_and(|ext| ext == "jsonl")
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.eq_ignore_ascii_case(id));
    if !name_matches || !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            tr!("Invalid session record path", "无效的会话记录路径"),
        ));
    }
    let parent = path.parent().unwrap();
    let bucket = parent.canonicalize()?;
    let metadata = fs::symlink_metadata(path)?;
    if bucket.parent() != Some(root)
        || bucket.file_name() != parent.file_name()
        || !metadata.is_file()
        || path.canonicalize()? != bucket.join(path.file_name().unwrap())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            tr!(
                "The session record is outside the expected directory",
                "会话记录位于预期目录之外"
            ),
        ));
    }
    Ok(bucket)
}

pub fn rename(session: &Session, title: &str) -> io::Result<()> {
    let root = mutation_root()?;
    let path = session.transcripts.first().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            tr!("Could not find the session record", "找不到会话记录"),
        )
    })?;
    check_record(&root, path, &session.id)?;
    append_title(path, &session.id, title)
}

fn append_title(path: &Path, id: &str, title: &str) -> io::Result<()> {
    let title = title.trim();
    if title.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            tr!("The session name cannot be empty", "会话名称不能为空"),
        ));
    }
    let mut file = OpenOptions::new().read(true).append(true).open(path)?;
    let size = file.seek(SeekFrom::End(0))?;
    let mut record = Vec::new();
    if size > 0 {
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0];
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            record.push(b'\n');
        }
    }
    serde_json::to_writer(
        &mut record,
        &serde_json::json!({
            "type": "custom-title",
            "customTitle": title,
            "sessionId": id,
        }),
    )?;
    record.push(b'\n');
    file.write_all(&record)
}

pub fn deletion_paths(sessions: &[Session]) -> io::Result<Vec<PathBuf>> {
    let root = mutation_root()?;
    collect_deletion_paths(&root, sessions)
}

fn collect_deletion_paths(root: &Path, sessions: &[Session]) -> io::Result<Vec<PathBuf>> {
    let mut known = HashSet::new();
    let mut paths = Vec::new();
    for session in sessions {
        for path in &session.transcripts {
            let canonical_bucket = match check_record(root, path, &session.id) {
                Ok(bucket) => bucket,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if !known.insert(path.clone()) {
                continue;
            }
            paths.push(path.clone());
            let companion = path.with_extension("");
            match fs::symlink_metadata(&companion) {
                Ok(metadata)
                    if metadata.is_dir()
                        && companion.canonicalize()?
                            == canonical_bucket.join(companion.file_name().unwrap()) =>
                {
                    paths.push(companion);
                }
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        tr!(
                            "Invalid supporting session data path",
                            "会话附属目录路径无效"
                        ),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(paths)
}

fn title_from(text: Option<&str>) -> Option<String> {
    let text = text?.trim();
    if text.is_empty() {
        return None;
    }
    let mut title = String::new();
    for character in text
        .split_whitespace()
        .flat_map(|word| word.chars().chain([' ']))
        .take(180)
    {
        title.push(character);
    }
    title.truncate(title.trim_end().len());
    Some(title)
}

fn prompt_title(content: &Value) -> Option<String> {
    fn visible(text: &str) -> bool {
        ![
            "<local-command",
            "<command-",
            "<system-reminder",
            "<task-notification",
            "<teammate-message",
        ]
        .iter()
        .any(|prefix| text.trim_start().starts_with(prefix))
    }
    if let Some(text) = content.as_str() {
        return title_from(Some(text).filter(|text| visible(text)));
    }
    content.as_array()?.iter().find_map(|block| {
        (block["type"].as_str() == Some("text")).then_some(())?;
        title_from(block["text"].as_str().filter(|text| visible(text)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_append_and_delete_selection_use_only_indexed_records() -> io::Result<()> {
        const ID: &str = "01234567-1234-1234-1234-0123456789ab";
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let workspace = env::temp_dir().join(format!(
            "claude-panel-fixture-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&workspace)?;
        let root = workspace.join("projects");
        let bucket = root.join("storage-bucket");
        let project_files = workspace.join("source-project");
        fs::create_dir_all(&bucket)?;
        fs::create_dir(&project_files)?;
        let log = bucket.join(format!("{ID}.jsonl"));
        let companion = bucket.join(ID);
        let original =
            br#"{"type":"user","cwd":"D:/example","message":{"content":"original message"}}"#;
        fs::write(&log, original)?;
        fs::create_dir(&companion)?;
        let foreign = project_files.join(format!("{ID}.jsonl"));
        fs::write(&foreign, original)?;

        append_title(&log, ID, "  改名后的会话  ")?;
        let bytes = fs::read(&log)?;
        assert!(bytes.starts_with(original));
        let session = parse(bytes.as_slice(), ID, SystemTime::UNIX_EPOCH, &log)?;
        assert_eq!(session.title, "改名后的会话");
        let root = root.canonicalize()?;
        assert_eq!(
            collect_deletion_paths(&root, std::slice::from_ref(&session))?,
            [log, companion]
        );
        let invalid = Session {
            transcripts: vec![foreign],
            ..session
        };
        assert!(collect_deletion_paths(&root, &[invalid]).is_err());
        fs::remove_dir_all(&workspace)
    }

    #[test]
    fn indexes_active_transcript_with_incomplete_tail() {
        let input = br#"{"type":"user","isMeta":true,"message":{"content":"metadata"}}
{"type":"user","cwd":"D:/dev/my-project","message":{"content":[{"type":"text","text":"first question"}]}}
{"type":"ai-title","aiTitle":"Generated title"}
{"type":"custom-title","customTitle":"Chosen title"}
{"type":"assistant","message":"#;
        let session = parse(
            &input[..],
            "01234567-1234-1234-1234-0123456789ab",
            SystemTime::UNIX_EPOCH,
            Path::new("fixture.jsonl"),
        )
        .unwrap();
        assert_eq!(session.title, "Chosen title");
        assert_eq!(session.cwd, Some(PathBuf::from("D:/dev/my-project")));
    }

    #[test]
    fn skips_command_records_and_uses_multiline_prompt() {
        let input =
            br#"{"type":"user","message":{"content":"<command-name>/resume</command-name>"}}
{"type":"user","message":{"content":"Fix the build\nPlease"}}
"#;
        let session = parse(
            &input[..],
            "01234567-1234-1234-1234-0123456789ab",
            SystemTime::UNIX_EPOCH,
            Path::new("fixture.jsonl"),
        )
        .unwrap();
        assert_eq!(session.title, "Fix the build Please");
    }
}
