use gio::prelude::*;
use mlua::{Lua, Table};
use std::{
    collections::HashSet,
    env,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Application {
    id: String,
    name: String,
    description: String,
    executable: String,
    icon: Option<String>,
}

pub fn table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    let applications = Arc::new(Mutex::new(Vec::new()));
    let searched = applications.clone();
    table.set(
        "search",
        lua.create_function(move |lua, (query, limit): (String, Option<usize>)| {
            let mut applications = searched.lock().unwrap();
            if query.is_empty() || applications.is_empty() {
                *applications = discover();
            }
            let results = search(&applications, &query, limit.unwrap_or(6).clamp(1, 512));
            let values = lua.create_table()?;
            for (index, application) in results.into_iter().enumerate() {
                let value = lua.create_table()?;
                value.set("id", application.id.as_str())?;
                value.set("name", application.name.as_str())?;
                value.set("description", application.description.as_str())?;
                value.set("executable", application.executable.as_str())?;
                value.set("icon", application.icon.as_deref())?;
                values.raw_set(index + 1, value)?;
            }
            Ok(values)
        })?,
    )?;
    table.set(
        "launch",
        lua.create_function(|_, id: String| {
            let application = gio::AppInfo::all()
                .into_iter()
                .find(|application| application.id().as_deref() == Some(id.as_str()))
                .ok_or_else(|| mlua::Error::external("application is no longer available"))?;
            application
                .launch(&[], None::<&gio::AppLaunchContext>)
                .map_err(mlua::Error::external)?;
            Ok(true)
        })?,
    )?;
    Ok(table)
}

#[derive(Default)]
pub struct QueryEditor {
    characters: Vec<char>,
    cursor: usize,
    anchor: Option<usize>,
}

impl QueryEditor {
    pub fn clear(&mut self) {
        self.characters.clear();
        self.cursor = 0;
        self.anchor = None;
    }

    pub fn insert(&mut self, character: char) -> bool {
        if self.selection().is_none() && self.characters.len() >= 256 {
            return false;
        }
        self.remove_selection();
        self.characters.insert(self.cursor, character);
        self.cursor += 1;
        true
    }

    pub fn backspace(&mut self, word: bool) -> bool {
        if self.remove_selection() {
            return true;
        }
        let start = if word {
            previous_word(&self.characters, self.cursor)
        } else {
            self.cursor.saturating_sub(1)
        };
        self.remove(start, self.cursor)
    }

    pub fn delete(&mut self, word: bool) -> bool {
        if self.remove_selection() {
            return true;
        }
        let end = if word {
            next_word(&self.characters, self.cursor)
        } else {
            (self.cursor + 1).min(self.characters.len())
        };
        self.remove(self.cursor, end)
    }

    pub fn left(&mut self, word: bool) -> bool {
        let cursor = if word {
            previous_word(&self.characters, self.cursor)
        } else {
            self.cursor.saturating_sub(1)
        };
        self.move_to(cursor)
    }

    pub fn right(&mut self, word: bool) -> bool {
        let cursor = if word {
            next_word(&self.characters, self.cursor)
        } else {
            (self.cursor + 1).min(self.characters.len())
        };
        self.move_to(cursor)
    }

    pub fn home(&mut self) -> bool {
        self.move_to(0)
    }

    pub fn end(&mut self) -> bool {
        self.move_to(self.characters.len())
    }

    pub fn select_all(&mut self) -> bool {
        if self.characters.is_empty() {
            return false;
        }
        self.anchor = Some(0);
        self.cursor = self.characters.len();
        true
    }

    pub fn text(&self) -> String {
        self.characters.iter().collect()
    }

    pub fn display(&self) -> String {
        let mut characters = self.characters.clone();
        characters.insert(self.cursor, '|');
        characters.iter().collect()
    }

    fn selection(&self) -> Option<(usize, usize)> {
        self.anchor
            .filter(|anchor| *anchor != self.cursor)
            .map(|anchor| (anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    fn remove_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.characters.drain(start..end);
        self.cursor = start;
        self.anchor = None;
        true
    }

    fn remove(&mut self, start: usize, end: usize) -> bool {
        if start == end {
            return false;
        }
        self.characters.drain(start..end);
        self.cursor = start;
        self.anchor = None;
        true
    }

    fn move_to(&mut self, cursor: usize) -> bool {
        let changed = self.cursor != cursor || self.anchor.is_some();
        self.cursor = cursor;
        self.anchor = None;
        changed
    }
}

fn previous_word(characters: &[char], mut cursor: usize) -> usize {
    while cursor > 0 && !characters[cursor - 1].is_alphanumeric() {
        cursor -= 1;
    }
    while cursor > 0 && characters[cursor - 1].is_alphanumeric() {
        cursor -= 1;
    }
    cursor
}

fn next_word(characters: &[char], mut cursor: usize) -> usize {
    while cursor < characters.len() && characters[cursor].is_alphanumeric() {
        cursor += 1;
    }
    while cursor < characters.len() && !characters[cursor].is_alphanumeric() {
        cursor += 1;
    }
    cursor
}

fn discover() -> Vec<Application> {
    let roots = data_roots();
    let mut seen = HashSet::new();
    let mut applications = gio::AppInfo::all()
        .into_iter()
        .filter(|application| application.should_show())
        .filter_map(|application| {
            let id = application.id()?.to_string();
            if !seen.insert(id.clone()) {
                return None;
            }
            Some(Application {
                id,
                name: application.display_name().to_string(),
                description: application.description().unwrap_or_default().to_string(),
                executable: application.executable().to_string_lossy().into_owned(),
                icon: application
                    .icon()
                    .and_then(|icon| resolve_icon(&icon, &roots)),
            })
        })
        .collect::<Vec<_>>();
    applications
        .sort_by_key(|application| (application.name.to_lowercase(), application.id.clone()));
    applications
}

fn search<'a>(applications: &'a [Application], query: &str, limit: usize) -> Vec<&'a Application> {
    let query = query.trim().to_lowercase();
    let mut matches = applications
        .iter()
        .filter_map(|application| {
            match_score(application, &query).map(|score| (score, application))
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|(score, application)| {
        (
            *score,
            application.name.to_lowercase(),
            application.id.clone(),
        )
    });
    matches
        .into_iter()
        .take(limit)
        .map(|(_, application)| application)
        .collect()
}

fn match_score(application: &Application, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let name = application.name.to_lowercase();
    if name == query {
        Some(0)
    } else if name.starts_with(query) {
        Some(1)
    } else if name
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| word.starts_with(query))
    {
        Some(2)
    } else if name.contains(query) {
        Some(3)
    } else if application.description.to_lowercase().contains(query)
        || application.executable.to_lowercase().contains(query)
    {
        Some(4)
    } else {
        None
    }
}

fn resolve_icon(icon: &gio::Icon, roots: &[PathBuf]) -> Option<String> {
    if let Some(icon) = icon.downcast_ref::<gio::FileIcon>() {
        return icon
            .file()
            .path()
            .filter(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned());
    }
    icon.downcast_ref::<gio::ThemedIcon>()?
        .names()
        .iter()
        .find_map(|name| find_themed_icon(roots, name))
        .map(|path| path.to_string_lossy().into_owned())
}

fn data_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    {
        roots.push(home);
    }
    let directories = env::var_os("XDG_DATA_DIRS")
        .map(|value| env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_else(|| {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        });
    roots.extend(directories);
    roots
}

fn find_themed_icon(roots: &[PathBuf], name: &str) -> Option<PathBuf> {
    icon_candidates(roots, name)
        .into_iter()
        .find(|path| path.is_file())
}

fn icon_candidates(roots: &[PathBuf], name: &str) -> Vec<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\']) {
        return Vec::new();
    }
    let name = [".png", ".svg", ".xpm"]
        .into_iter()
        .find_map(|extension| name.strip_suffix(extension))
        .unwrap_or(name);
    let mut candidates = Vec::new();
    for root in roots {
        for directory in ["48x48", "64x64", "scalable", "128x128", "32x32", "256x256"] {
            for extension in ["png", "svg", "xpm"] {
                candidates.push(
                    root.join("icons/hicolor")
                        .join(directory)
                        .join("apps")
                        .join(format!("{name}.{extension}")),
                );
            }
        }
        for extension in ["png", "svg", "xpm"] {
            candidates.push(root.join("pixmaps").join(format!("{name}.{extension}")));
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn app(id: &str, name: &str, description: &str, executable: &str) -> Application {
        Application {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            executable: executable.into(),
            icon: None,
        }
    }

    #[test]
    fn search_is_ranked_stable_and_bounded() {
        let applications = vec![
            app("z", "Beta Alpha", "", "beta"),
            app("b", "Alpha", "", "alpha"),
            app("a", "Alpha Tool", "", "tool"),
            app("c", "My Alpha", "", "my-alpha"),
            app("d", "Other", "alpha editor", "other"),
        ];
        let ids = search(&applications, "alpha", 4)
            .into_iter()
            .map(|application| application.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["b", "a", "z", "c"]);
        assert_eq!(search(&applications, "alpha", 2).len(), 2);
    }

    #[test]
    fn query_editor_handles_unicode_and_control_motions() {
        let mut query = QueryEditor::default();
        for character in "hello café🦀".chars() {
            query.insert(character);
        }
        assert!(query.backspace(false));
        assert_eq!(query.text(), "hello café");
        assert!(query.left(true));
        assert!(query.backspace(true));
        assert_eq!(query.text(), "café");
        assert!(query.right(true));
        assert!(query.select_all());
        assert!(query.insert('x'));
        assert_eq!(query.text(), "x");
    }

    #[test]
    fn hicolor_icon_candidates_prefer_48_pixels_then_pixmaps() {
        let roots = [PathBuf::from("/one"), PathBuf::from("/two")];
        let candidates = icon_candidates(&roots, "demo");
        assert_eq!(
            candidates[0],
            Path::new("/one/icons/hicolor/48x48/apps/demo.png")
        );
        assert!(
            candidates
                .iter()
                .position(|path| path == Path::new("/one/pixmaps/demo.png"))
                .unwrap()
                < candidates
                    .iter()
                    .position(|path| path == Path::new("/two/icons/hicolor/48x48/apps/demo.png"))
                    .unwrap()
        );
    }
}
