//! Panel filter matching: the bounded per-thread search cache and the three
//! entry points that turn a typed filter into matching item indices.
//!
//! Split out of `panel_render.rs` because filtering is a query boundary rather
//! than a rendering one. Every panel kind - session, message, model, subagent,
//! file - asks the same question of the same `(items, descriptions, groups,
//! filter)` shape, and the three cache constants exist to keep that one shared
//! question cheap without ever changing what it answers.

use super::*;

pub(super) const PANEL_SEARCH_CACHE_MAX_ITEMS: usize = 4096;
pub(super) const PANEL_SEARCH_CACHE_MAX_BYTES: usize = 2 * 1024 * 1024;
pub(super) const PANEL_SEARCH_CACHE_MAX_SOURCE_BYTES: usize = PANEL_SEARCH_CACHE_MAX_BYTES / 4;

struct CachedPanelSearchItem {
    label: String,
    description: Option<String>,
    group: Option<String>,
    normalized: String,
    /// Present only for a model picker snapshot.
    separator_free: Option<SeparatorFreeFields>,
}

/// Whether a term may also match a label or provider with separators left out.
/// Only model pickers opt in (#726); every other panel stays literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SeparatorMatching {
    Literal,
    ModelNames,
}

/// A row's label and provider, each lowercased and then stripped of
/// separators and whitespace. The two stay separate so one term can never
/// match across the end of one field and the start of the other. Descriptions
/// are left out on purpose: they are prose and keep literal matching.
struct SeparatorFreeFields {
    label: String,
    group: Option<String>,
}

impl SeparatorFreeFields {
    fn new(label: &str, group: Option<&str>) -> Self {
        Self {
            label: without_separators(&label.to_lowercase()),
            group: group.map(|group| without_separators(&group.to_lowercase())),
        }
    }

    fn contains(&self, term: &str) -> bool {
        self.label.contains(term)
            || self
                .group
                .as_deref()
                .is_some_and(|group| group.contains(term))
    }

    fn retained_bytes(&self) -> usize {
        self.label.capacity() + self.group.as_ref().map_or(0, String::capacity)
    }
}

fn is_name_separator(c: char) -> bool {
    matches!(c, '-' | '_' | '.' | '/' | ':')
}

/// Remove name separators and whitespace from already lowercased text.
fn without_separators(lowercased: &str) -> String {
    let mut stripped = String::with_capacity(lowercased.len());
    stripped.extend(
        lowercased
            .chars()
            .filter(|c| !is_name_separator(*c) && !c.is_whitespace()),
    );
    stripped
}

/// One whitespace-delimited filter term, prepared once per filtering call.
struct SearchTerm {
    literal: String,
    /// The term without separators. `None` under literal matching, and when
    /// nothing is left (a term such as `-`): an empty needle would match every
    /// row, so such a term keeps literal matching only.
    separator_free: Option<String>,
}

impl SearchTerm {
    fn new(term: &str, matching: SeparatorMatching) -> Self {
        let literal = term.to_lowercase();
        let separator_free = (matching == SeparatorMatching::ModelNames)
            .then(|| without_separators(&literal))
            .filter(|stripped| !stripped.is_empty());
        Self {
            literal,
            separator_free,
        }
    }
}

pub(super) struct PanelSearchCache {
    items: Vec<CachedPanelSearchItem>,
    matching: SeparatorMatching,
}

impl PanelSearchCache {
    fn matches(
        &self,
        items: &[String],
        descriptions: &[Option<String>],
        groups: Option<&[String]>,
        matching: SeparatorMatching,
    ) -> bool {
        self.matching == matching
            && self.items.len() == items.len()
            && self.items.iter().enumerate().all(|(index, cached)| {
                cached.label == items[index]
                    && cached.description.as_deref()
                        == descriptions.get(index).and_then(Option::as_deref)
                    && cached.group.as_deref()
                        == groups
                            .and_then(|groups| groups.get(index))
                            .map(String::as_str)
            })
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.items.capacity() * std::mem::size_of::<CachedPanelSearchItem>()
            + self
                .items
                .iter()
                .map(|item| {
                    item.label.capacity()
                        + item.description.as_ref().map_or(0, String::capacity)
                        + item.group.as_ref().map_or(0, String::capacity)
                        + item.normalized.capacity()
                        + item
                            .separator_free
                            .as_ref()
                            .map_or(0, SeparatorFreeFields::retained_bytes)
                })
                .sum::<usize>()
    }

    fn build(
        items: &[String],
        descriptions: &[Option<String>],
        groups: Option<&[String]>,
        matching: SeparatorMatching,
    ) -> Option<Self> {
        if items.len() > PANEL_SEARCH_CACHE_MAX_ITEMS {
            return None;
        }
        // Check borrowed source sizes before cloning or normalizing. Oversized
        // panels still search every item through the uncached path below.
        let mut source_bytes = 0usize;
        for (index, item) in items.iter().enumerate() {
            source_bytes = source_bytes
                .checked_add(item.len())?
                .checked_add(
                    descriptions
                        .get(index)
                        .and_then(Option::as_ref)
                        .map_or(0, String::len),
                )?
                .checked_add(
                    groups
                        .and_then(|groups| groups.get(index))
                        .map_or(0, String::len),
                )?;
            if source_bytes > PANEL_SEARCH_CACHE_MAX_SOURCE_BYTES {
                return None;
            }
        }
        let cached = Self {
            matching,
            items: items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let description = descriptions.get(index).and_then(Option::as_deref);
                    let group = groups
                        .and_then(|groups| groups.get(index))
                        .map(String::as_str);
                    CachedPanelSearchItem {
                        label: item.clone(),
                        description: description.map(str::to_owned),
                        group: group.map(str::to_owned),
                        normalized: normalized_panel_search_text(item, description, group),
                        separator_free: (matching == SeparatorMatching::ModelNames)
                            .then(|| SeparatorFreeFields::new(item, group)),
                    }
                })
                .collect(),
        };
        (cached.retained_bytes() <= PANEL_SEARCH_CACHE_MAX_BYTES).then_some(cached)
    }
}

thread_local! {
    // Input and rendering can run on different threads. Each retains at most one
    // bounded snapshot. Exact source equality, not addresses or a hash, is the
    // cache identity, so in-place updates and replacement panels cannot go stale.
    pub(super) static PANEL_SEARCH_CACHE: std::cell::RefCell<Option<PanelSearchCache>> = const {
        std::cell::RefCell::new(None)
    };
}

fn normalized_panel_search_text(
    label: &str,
    description: Option<&str>,
    group: Option<&str>,
) -> String {
    #[cfg(test)]
    panel_render_test_hook::record_search_normalization();
    let mut searchable = label.to_lowercase();
    for field in [description, group].into_iter().flatten() {
        searchable.push(' ');
        searchable.push_str(&field.to_lowercase());
    }
    searchable
}

/// Literal filtering, as every panel other than a model picker uses it.
#[cfg(test)]
pub(super) fn filtered_indices_with_groups(
    items: &[String],
    descriptions: &[Option<String>],
    groups: Option<&[String]>,
    filter: &str,
) -> Vec<usize> {
    filtered_indices(
        items,
        descriptions,
        groups,
        filter,
        SeparatorMatching::Literal,
    )
}

/// Indices of the items matching the current filter, in their original order.
/// Every whitespace-delimited term must appear in the label, description, or
/// provider, case-insensitively. Under [`SeparatorMatching::ModelNames`] a term
/// also matches when, with `-`, `_`, `.`, `/` and `:` removed, it appears in
/// the label or in the provider with those separators and whitespace removed,
/// so `gpt6.1` finds `gpt-6.1-Sol`.
fn filtered_indices(
    items: &[String],
    descriptions: &[Option<String>],
    groups: Option<&[String]>,
    filter: &str,
    matching: SeparatorMatching,
) -> Vec<usize> {
    let needles = filter
        .split_whitespace()
        .map(|term| SearchTerm::new(term, matching))
        .collect::<Vec<_>>();
    if needles.is_empty() {
        return (0..items.len()).collect();
    }
    PANEL_SEARCH_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if !cache
            .as_ref()
            .is_some_and(|cache| cache.matches(items, descriptions, groups, matching))
        {
            *cache = PanelSearchCache::build(items, descriptions, groups, matching);
        }
        items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let group = groups
                    .and_then(|groups| groups.get(index))
                    .map(String::as_str);
                let uncached;
                let uncached_separator_free;
                let (searchable, separator_free) = if let Some(cache) = cache.as_ref() {
                    let cached = &cache.items[index];
                    (&cached.normalized, cached.separator_free.as_ref())
                } else {
                    // Oversized panels are searched without the cache and must
                    // return the same rows as the cached path.
                    uncached = normalized_panel_search_text(
                        item,
                        descriptions.get(index).and_then(Option::as_deref),
                        group,
                    );
                    uncached_separator_free = (matching == SeparatorMatching::ModelNames)
                        .then(|| SeparatorFreeFields::new(item, group));
                    (&uncached, uncached_separator_free.as_ref())
                };
                needles
                    .iter()
                    .all(|needle| {
                        searchable.contains(&needle.literal)
                            || needle
                                .separator_free
                                .as_deref()
                                .zip(separator_free)
                                .is_some_and(|(term, fields)| fields.contains(term))
                    })
                    .then_some(index)
            })
            .collect()
    })
}

/// Indices the typed filter matched, before any presentation grouping hides
/// them. Group counts and collapsed summaries must be measured here: measured
/// against the already-collapsed set, every collapsed terminal group reports
/// zero members and the reader is told nothing about the workers it hides.
pub(in crate::tui::view) fn searched_indices_for_action(
    items: &[String],
    descriptions: &[Option<String>],
    action: &PanelAction,
    filter: &str,
) -> Vec<usize> {
    let matching = if action.is_model_picker() {
        SeparatorMatching::ModelNames
    } else {
        SeparatorMatching::Literal
    };
    filtered_indices(
        items,
        descriptions,
        action.model_provider_groups(),
        filter,
        matching,
    )
}

pub(in crate::tui::view) fn filtered_indices_for_action(
    items: &[String],
    descriptions: &[Option<String>],
    action: &PanelAction,
    filter: &str,
) -> Vec<usize> {
    let mut indices = searched_indices_for_action(items, descriptions, action, filter);
    if let PanelAction::SelectGroupedModel {
        providers,
        scope: Some(scope),
        ..
    } = action
    {
        indices.retain(|index| providers.get(*index) == Some(scope));
    }
    // Terminal subagent groups collapse behind their heading. A typed filter
    // still searches every worker, hidden groups included, so filtering for a
    // finished worker can never look like the worker disappeared.
    if let Some(subagents) = action.subagent_panel() {
        if filter.is_empty() {
            indices.retain(|index| !subagents.hides(*index));
        }
        // An active state view filter is an explicit narrowing: it restricts
        // the visible rows whether or not a typed filter is present.
        if let Some(active) = subagents.state_filter_label() {
            indices.retain(|index| subagents.group_label(*index) == Some(active));
        }
    }
    indices
}

fn normalize_search_text(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn session_search_text(meta: &crate::session_store::SessionMeta) -> String {
    format!(
        "{} {} {} {} {} {}",
        meta.id,
        meta.name.as_deref().unwrap_or_default(),
        meta.title,
        meta.tags.join(" "),
        meta.path.display(),
        meta.workspace
            .as_deref()
            .map_or_else(String::new, |workspace| workspace.display().to_string()),
    )
}

pub(super) fn match_session_search(
    meta: &crate::session_store::SessionMeta,
    query: &crate::tui::fuzzy::ParsedSearchQuery,
) -> Option<f64> {
    if query.error.is_some() {
        return None;
    }
    if query.is_empty() {
        return Some(0.0);
    }
    let haystack = session_search_text(meta);
    match query.mode {
        SearchMode::Regex => {
            let regex = query.regex.as_ref()?;
            regex
                .find(&haystack)
                .map(|matched| matched.start() as f64 * 0.1)
        }
        SearchMode::Tokens => {
            let mut score = 0.0;
            let mut normalized = None;
            for token in &query.tokens {
                match token.kind {
                    TokenKind::Phrase => {
                        let normalized_haystack =
                            normalized.get_or_insert_with(|| normalize_search_text(&haystack));
                        let phrase = normalize_search_text(&token.value);
                        if phrase.is_empty() {
                            continue;
                        }
                        let position = normalized_haystack.find(&phrase)?;
                        score += position as f64 * 0.1;
                    }
                    TokenKind::Fuzzy => {
                        let matched = fuzzy_match(&token.value, &haystack);
                        if !matched.matches {
                            return None;
                        }
                        score += matched.score;
                    }
                }
            }
            Some(score)
        }
    }
}
