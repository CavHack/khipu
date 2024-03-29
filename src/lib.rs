#[macro_use] extern crate maplit;

#[macro_use]extern crate lazy_static;
extern crate regex;
extern crate unicode_normalization;

mod regexbuilder;
mod util;

pub mod error;
#[cfg(feature = "runtime")]
pub mod rt;
pub mod filters;

mod Brotli;
mod glyph;

pub use oauth::Credentials;

pub use crate::error::Error;
pub use crate::glyph::Glyph;

use std::borrow::Borrow;
use std::future::Future;
use std::marker::Unpin;
use std::pin::Pin;
use std::task::{Context, Poll};
#[cfg(feature = "runtime")]
use std::time::Duration;

use bytes::Bytes;
use futures_core::Stream;
use futures_util::{ready, FutureExt, StreamExt};
use http::response::Parts;
use hyper::body::{Body, Payload};
use hyper::client::connect::Connect;
use hyper::client::{Client, ResponseFuture};
use hyper::header::{
    HeaderValue, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE,
};
use hyper::Request;
use string::TryFrom;

use crate::Brotli::MaybeBrotli;
use crate::filters::{FilterLevel, RequestMethod, StatusCode, Uri};
use crate::util::*;



/// A streamBuilder for `TwitterStream`.
///
/// ## Example
///
/// ```rust,no_run
/// use futures::prelude::*;
/// use khipu::Glyph;
///
/// # #[tokio::main]
/// # async fn main() {
/// let glyph = Glyph::new("consumer_key", "consumer_secret", "access_key", "access_secret");
///
/// khipu::StreamBuilder::sample(glyph)
///     .timeout(None)
///     .listen()
///     .unwrap()
///     .try_flatten_stream()
///     .try_for_each(|json| {
///         println!("{}", json);
///         future::ok(())
///     })
///     .await
///     .unwrap();
/// # }
/// ```

const B_INCR: f64 =  0.293;
const B_DECR: f64 = -0.293;

const C_INCR:   f64 =  0.733;
const NEGATION_SCALAR: f64 = -0.740;

//sentiment increases for text with question or exclamation marks
const QMARK_INCR: f64 = 0.180;
const EMARK_INCR: f64 = 0.292;

//Maximum amount of question or question marks before their contribution to sentiment is
//disregarded
const MAX_EMARK: i32 = 4;
const MAX_QMARK: i32 = 3;
const MAX_QMARK_INCR: f64 = 0.96;

const NORMALIZATION_ALPHA: f64 = 15.0;

static NEGATION_glyphS: [&'static str; 59] =
    ["aint", "arent", "cannot", "cant", "couldnt", "darent", "didnt", "doesnt",
        "ain't", "aren't", "can't", "couldn't", "daren't", "didn't", "doesn't",
        "dont", "hadnt", "hasnt", "havent", "isnt", "mightnt", "mustnt", "neither",
        "don't", "hadn't", "hasn't", "haven't", "isn't", "mightn't", "mustn't",
        "neednt", "needn't", "never", "none", "nope", "nor", "not", "nothing", "nowhere",
        "oughtnt", "shant", "shouldnt", "uhuh", "wasnt", "werent",
        "oughtn't", "shan't", "shouldn't", "uh-uh", "wasn't", "weren't",
        "without", "wont", "wouldnt", "won't", "wouldn't", "rarely", "seldom", "despite"];

static RAW_LEXICON: &'static str = include_str!("resources/vader_lexicon.txt");
static RAW_EMOJI_LEXICON: &'static str = include_str!("resources/emoji_utf8_lexicon.txt");

lazy_static! {
    static ref BOOSTER_DICT: HashMap<&'static str, f64> =  hashmap![
         "absolutely" => B_INCR, "amazingly" => B_INCR, "awfully" => B_INCR, "completely" => B_INCR, "considerably" => B_INCR,
         "decidedly" => B_INCR, "deeply" => B_INCR, "effing" => B_INCR, "enormously" => B_INCR,
         "entirely" => B_INCR, "especially" => B_INCR, "exceptionally" => B_INCR, "extremely" => B_INCR,
         "fabulously" => B_INCR, "flipping" => B_INCR, "flippin" => B_INCR,
         "fricking" => B_INCR, "frickin" => B_INCR, "frigging" => B_INCR, "friggin" => B_INCR, "fully" => B_INCR, "fucking" => B_INCR,
         "greatly" => B_INCR, "hella" => B_INCR, "highly" => B_INCR, "hugely" => B_INCR, "incredibly" => B_INCR,
         "intensely" => B_INCR, "majorly" => B_INCR, "more" => B_INCR, "most" => B_INCR, "particularly" => B_INCR,
         "purely" => B_INCR, "quite" => B_INCR, "really" => B_INCR, "remarkably" => B_INCR,
         "so" => B_INCR, "substantially" => B_INCR,
         "thoroughly" => B_INCR, "totally" => B_INCR, "tremendously" => B_INCR,
         "uber" => B_INCR, "unbelievably" => B_INCR, "unusually" => B_INCR, "utterly" => B_INCR,
         "very" => B_INCR,
         "almost" => B_DECR, "barely" => B_DECR, "hardly" => B_DECR, "just enough" => B_DECR,
         "kind of" => B_DECR, "kinda" => B_DECR, "kindof" => B_DECR, "kind-of" => B_DECR,
         "less" => B_DECR, "little" => B_DECR, "marginally" => B_DECR, "occasionally" => B_DECR, "partly" => B_DECR,
         "scarcely" => B_DECR, "slightly" => B_DECR, "somewhat" => B_DECR,
         "sort of" => B_DECR, "sorta" => B_DECR, "sortof" => B_DECR, "sort-of" => B_DECR];

    /**
     * These dicts were used in some WIP or planned features in the original
     * I may implement them later if I can understand how they're intended to work
     **/

    // // check for sentiment laden idioms that do not contain lexicon words (future work, not yet implemented)
    // static ref SENTIMENT_LADEN_IDIOMS: HashMap<&'static str, f64> = hashmap![
    //      "cut the mustard" => 2.0, "hand to mouth" => -2.0,
    //      "back handed" => -2.0, "blow smoke" => -2.0, "blowing smoke" => -2.0,
    //      "upper hand" => 1.0, "break a leg" => 2.0,
    //      "cooking with gas" => 2.0, "in the black" => 2.0, "in the red" => -2.0,
    //      "on the ball" => 2.0, "under the weather" => -2.0];


    // check for special case idioms containing lexicon words
    static ref SPECIAL_CASE_IDIOMS: HashMap<&'static str, f64> = hashmap![
         "the shit" => 3.0, "the bomb" => 3.0, "bad ass" => 1.5, "yeah right" => -2.0,
         "kiss of death" => -1.5];

    static ref ALL_CAPS_RE: Regex = Regex::new(r"^[A-Z\W]+$").unwrap();

    static ref PUNCTUATION: &'static str = "[!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~]";

    pub static ref LEXICON: HashMap<&'static str, f64> = parse_raw_lexicon(RAW_LEXICON);
    pub static ref EMOJI_LEXICON: HashMap<&'static str, &'static str> = parse_raw_emoji_lexicon(RAW_EMOJI_LEXICON);
}



///A convenience macro to break loops if the given value is `None`.
macro_rules! break_opt {
    ($input:expr) => {{
        if let Some(val) = $input {
            val
        }
        else { break; }
    }};
}

///A convenience macro to continue loops if the given value is `None`.
macro_rules! continue_opt {
    ($input:expr) => {{
        if let Some(val) = $input {
            val
        }
        else { continue; }
    }};
}

///A convenience macro to unwrap a given Option or return None from the containining function.
macro_rules! try_opt {
    ($input:expr) => {{
        if let Some(val) = $input {
            val
        }
        else { return None; }
    }};
}

///Represents the kinds of entities that can be extracted from a given text.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum EntityKind {
    ///A URL.
    Url,
    ///A user mention.
    ScreenName,
    ///A list mention, in the form "@user/list-name".
    ListName,
    ///A hashtag.
    Hashtag,
    ///A financial symbol ("cashtag").
    Symbol,
}

///Represents an entity extracted from a given text.
///
///This struct is meant to be returned from the entity parsing functions and linked to the source
///string that was parsed from the function in question. This is because the Entity struct itself
///only contains byte offsets for the string in question.
///
///# Examples
///
///To load the string in question, you can use the byte offsets directly, or use the `substr`
///method on the Entity itself:
///
///```rust
/// use egg_mode_text::hashtag_entities;
///
/// let text = "this is a #hashtag";
/// let results = hashtag_entities(text, true);
/// let entity = results.first().unwrap();
///
/// assert_eq!(&text[entity.range.0..entity.range.1], "#hashtag");
/// assert_eq!(entity.substr(text), "#hashtag");
///```
///
///Just having the byte offsets may seem like a roundabout way to store the extracted string, but
///with the byte offsets, you can also substitute in text decoration, like HTML links:
///
///```rust
/// use egg_mode_text::hashtag_entities;
///
/// let text = "this is a #hashtag";
/// let results = hashtag_entities(text, true);
/// let mut output = String::new();
/// let mut last_pos = 0;
///
/// for entity in results {
///     output.push_str(&text[last_pos..entity.range.0]);
///     //NOTE: this doesn't URL-encode the hashtag for the link
///     let tag = entity.substr(text);
///     let link = format!("<a href='https://twitter.com/#!/search?q={0}'>{0}</a>", tag);
///     output.push_str(&link);
///     last_pos = entity.range.1;
/// }
/// output.push_str(&text[last_pos..]);
///
/// assert_eq!(output, "this is a <a href='https://twitter.com/#!/search?q=#hashtag'>#hashtag</a>");
///```
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub struct Entity {
    ///The kind of entity that was extracted.
    pub kind: EntityKind,
    ///The byte offsets between which the entity text is. The first index indicates the byte at the
    ///beginning of the extracted entity, but the second one is the byte index for the first
    ///character after the extracted entity (or one past the end of the string if the entity was at
    ///the end of the string). For hashtags and symbols, the range includes the # or $ character.
    pub range: (usize, usize),
}

impl Entity {
    ///Returns the substring matching this entity's byte offsets from the given text.
    ///
    ///# Panics
    ///
    ///This function will panic if the byte offsets in this entity do not match codepoint
    ///boundaries in the given text. This can happen if the text is not the original string that
    ///this entity was parsed from.
    pub fn substr<'a>(&self, text: &'a str) -> &'a str {
        &text[self.range.0..self.range.1]
    }
}

///Parses the given string for all entities: URLs, hashtags, financial symbols ("cashtags"), user
///mentions, and list mentions.
///
///This function is a shorthand for calling `url_entities`, `mention_list_entities`,
///`hashtag_entities`, and `symbol_entities` before merging the results together into a single Vec.
///The output is sorted so that entities are in that order (and individual kinds are ordered
///according to their appearance within the string) before exiting.
///
///# Example
///
///```rust
/// use egg_mode_text::{EntityKind, entities};
///
/// let text = "sample #text with a link to twitter.com";
/// let mut results = entities(text).into_iter();
///
/// let entity = results.next().unwrap();
/// assert_eq!(entity.kind, EntityKind::Url);
/// assert_eq!(entity.substr(text), "twitter.com");
///
/// let entity = results.next().unwrap();
/// assert_eq!(entity.kind, EntityKind::Hashtag);
/// assert_eq!(entity.substr(text), "#text");
///
/// assert_eq!(results.next(), None);
///```
pub fn entities(text: &str) -> Vec<Entity> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut results = url_entities(text);

    let urls = results.clone();

    results.extend(extract_hashtags(text, &urls));
    results.extend(extract_symbols(text, &urls));

    for mention in mention_list_entities(text) {
        let mut found = false;

        for existing in &results {
            if mention.range.0 <= existing.range.1 && existing.range.0 <= mention.range.1 {
                found = true;
                break;
            }
        }

        if !found {
            results.push(mention);
        }
    }

    results.sort();
    results
}

///Parses the given string for URLs.
///
///The entities returned from this function can be used to determine whether a url will be
///automatically shortened with a t.co link (in fact, this function is called from
///`character_count`), or to automatically add hyperlinks to URLs in a text if it hasn't been sent
///to Twitter yet.
///
///# Example
///
///```rust
/// use egg_mode_text::url_entities;
///
/// let text = "sample text with a link to twitter.com and one to rust-lang.org as well";
/// let mut results = url_entities(text).into_iter();
///
/// let entity = results.next().unwrap();
/// assert_eq!(entity.substr(text), "twitter.com");
///
/// let entity = results.next().unwrap();
/// assert_eq!(entity.substr(text), "rust-lang.org");
///
/// assert_eq!(results.next(), None);
///```
pub fn url_entities(text: &str) -> Vec<Entity> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut results: Vec<Entity> = Vec::new();
    let mut cursor = 0;

    while cursor < text.len() {
        let substr = &text[cursor..];
        let current_cursor = cursor;

        let caps = break_opt!(regexen::RE_SIMPLIFIED_VALID_URL.captures(substr));
        if caps.len() < 9 {
            break;
        }

        cursor += caps.pos(0).unwrap().1;

        let preceding_text = caps.at(2);
        let url_range = caps.pos(3);
        let protocol_range = caps.pos(4);
        let domain_range = caps.pos(5);
        let path_range = caps.pos(7);

        //if protocol is missing and domain contains non-ascii chars, extract ascii-only
        //domains.
        if protocol_range.is_none() {
            if let Some(preceding) = preceding_text {
                if !preceding.is_empty() && regexen::RE_URL_WO_PROTOCOL_INVALID_PRECEDING_CHARS.is_match(preceding) {
                    continue;
                }
            }

            let mut domain_range = continue_opt!(domain_range);

            let mut loop_inserted = false;

            while domain_range.0 < domain_range.1 {
                //include succeeding character for validation
                let extra_char = if let Some(ch) = substr[domain_range.1..].chars().next() {
                    ch.len_utf8()
                }
                else {
                    0
                };

                let domain_test = &substr[domain_range.0..(domain_range.1+extra_char)];
                let caps = break_opt!(regexen::RE_VALID_ASCII_DOMAIN.captures(domain_test));
                let url_range = break_opt!(caps.pos(1));
                let ascii_url = &domain_test[url_range.0..url_range.1];

                if path_range.is_some() ||
                   regexen::RE_VALID_SPECIAL_SHORT_DOMAIN.is_match(ascii_url) ||
                   !regexen::RE_INVALID_SHORT_DOMAIN.is_match(ascii_url)
                {
                    loop_inserted = true;

                    results.push(Entity {
                        kind: EntityKind::Url,
                        range: (current_cursor + domain_range.0 + url_range.0,
                                current_cursor + domain_range.0 + url_range.1),
                    });
                }

                domain_range.0 += url_range.1;
            }

            if !loop_inserted {
                continue;
            }

            if let Some(last_entity) = results.last_mut() {
                if let Some(path_range) = path_range {
                    if last_entity.range.1 == (current_cursor + path_range.0) {
                        last_entity.range.1 += path_range.1 - path_range.0;
                    }
                }

                cursor = last_entity.range.1;
            }
        }
        else {
            let mut url_range = continue_opt!(url_range);
            let domain_range = continue_opt!(domain_range);

            //in case of t.co URLs, don't allow additional path characters
            if let Some((_, to)) = regexen::RE_VALID_TCO_URL.find(&substr[url_range.0..url_range.1]) {
                url_range.1 = url_range.0 + to;
            }
            else if !regexen::RE_URL_FOR_VALIDATION.is_match(&substr[domain_range.0..domain_range.1]) {
                continue;
            }

            results.push(Entity {
                kind: EntityKind::Url,
                range: (current_cursor + url_range.0,
                        current_cursor + url_range.1),
            });
        }
    }

    results
}

///Parses the given string for user and list mentions.
///
///As the parsing rules for user mentions and list mentions, this function is able to extract both
///kinds at once. To differentiate between the two, check the entity's `kind` field.
///
///The entities returned by this function can be used to find mentions for hyperlinking, as well as
///to provide an autocompletion facility, if the byte-offset position of the cursor is known with
///relation to the full text.
///
///# Example
///
///```rust
/// use egg_mode_text::{EntityKind, mention_list_entities};
///
/// let text = "sample text with a mention for @twitter and a link to @rustlang/fakelist";
/// let mut results = mention_list_entities(text).into_iter();
///
/// let entity = results.next().unwrap();
/// assert_eq!(entity.kind, EntityKind::ScreenName);
/// assert_eq!(entity.substr(text), "@twitter");
///
/// let entity = results.next().unwrap();
/// assert_eq!(entity.kind, EntityKind::ListName);
/// assert_eq!(entity.substr(text), "@rustlang/fakelist");
///
/// assert_eq!(results.next(), None);
///```
pub fn mention_list_entities(text: &str) -> Vec<Entity> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut cursor = 0usize;

    loop {
        if cursor >= text.len() {
            break;
        }

        //save our matching substring since we modify cursor below
        let substr = &text[cursor..];

        let caps = break_opt!(regexen::RE_VALID_MENTION_OR_LIST.captures(substr));

        if caps.len() < 5 {
            break;
        }

        let current_cursor = cursor;
        cursor += caps.pos(0).unwrap().1;

        if !regexen::RE_END_MENTION.is_match(&text[cursor..]) {
            let at_sign_range = continue_opt!(caps.pos(2));
            let screen_name_range = caps.pos(3);
            let list_name_range = caps.pos(4);

            if let Some((_, end)) = list_name_range {
                results.push(Entity {
                    kind: EntityKind::ListName,
                    range: (current_cursor + at_sign_range.0, current_cursor + end),
                });
            }
            else if let Some((_, end)) = screen_name_range {
                results.push(Entity {
                    kind: EntityKind::ScreenName,
                    range: (current_cursor + at_sign_range.0, current_cursor + end),
                });
            }
        }
        else {
            //Avoid matching the second username in @username@username
            cursor += if let Some(ch) = text[cursor..].chars().next() {
                ch.len_utf8()
            }
            else {
                1
            };
        }
    }

    results
}

///Parses the given string for user mentions.
///
///This is given as a convenience function for uses where mentions are needed but list mentions are
///not. This function effectively returns the same set as `mention_list_entities` but with list
///mentions removed.
///
///# Example
///
///```rust
/// use egg_mode_text::{EntityKind, mention_entities};
///
/// let text = "sample text with a mention for @twitter and a link to @rustlang/fakelist";
/// let mut results = mention_entities(text).into_iter();
///
/// let entity = results.next().unwrap();
/// assert_eq!(entity.kind, EntityKind::ScreenName);
/// assert_eq!(entity.substr(text), "@twitter");
///
/// assert_eq!(results.next(), None);
///```
pub fn mention_entities(text: &str) -> Vec<Entity> {
    let mut results = mention_list_entities(text);

    results.retain(|e| e.kind == EntityKind::ScreenName);

    results
}

///Parses the given string for a user mention at the beginning of the text, if present.
///
///This function is provided as a convenience method to see whether the given text counts as a
///tweet reply. If this function returns `Some` for a given draft tweet, then the final tweet is
///counted as a direct reply.
///
///Note that the entity returned by this function does not include the @-sign at the beginning of
///the mention.
///
///# Examples
///
///```rust
/// use egg_mode_text::reply_mention_entity;
///
/// let text = "@rustlang this is a reply";
/// let reply = reply_mention_entity(text).unwrap();
/// assert_eq!(reply.substr(text), "rustlang");
///
/// let text = ".@rustlang this is not a reply";
/// assert_eq!(reply_mention_entity(text), None);
///```
pub fn reply_mention_entity(text: &str) -> Option<Entity> {
    if text.is_empty() {
        return None;
    }

    let caps = try_opt!(regexen::RE_VALID_REPLY.captures(text));
    if caps.len() < 2 {
        return None;
    }

    let reply_range = try_opt!(caps.pos(1));

    if regexen::RE_END_MENTION.is_match(&text[reply_range.1..]) {
        return None;
    }

    Some(Entity {
        kind: EntityKind::ScreenName,
        range: reply_range,
    })
}

///Parses the given string for hashtags, optionally leaving out those that are part of URLs.
///
///The entities returned by this function can be used to find hashtags for hyperlinking, as well as
///to provide an autocompletion facility, if the byte-offset position of the cursor is known with
///relation to the full text.
///
///# Example
///
///With the `check_url_overlap` parameter, you can make sure you don't include text anchors from
///URLs:
///
///```rust
/// use egg_mode_text::hashtag_entities;
///
/// let text = "some #hashtag with a link to twitter.com/#anchor";
/// let mut results = hashtag_entities(text, true).into_iter();
///
/// let tag = results.next().unwrap();
/// assert_eq!(tag.substr(text), "#hashtag");
///
/// assert_eq!(results.next(), None);
///```
///
///If you pass `false` for that parameter, it won't parse for URLs to check for overlap:
///
///```rust
/// use egg_mode_text::hashtag_entities;
///
/// let text = "some #hashtag with a link to twitter.com/#anchor";
/// let mut results = hashtag_entities(text, false).into_iter();
///
/// let tag = results.next().unwrap();
/// assert_eq!(tag.substr(text), "#hashtag");
///
/// let tag = results.next().unwrap();
/// assert_eq!(tag.substr(text), "#anchor");
///
/// assert_eq!(results.next(), None);
///```
pub fn hashtag_entities(text: &str, check_url_overlap: bool) -> Vec<Entity> {
    if text.is_empty() {
        return Vec::new();
    }

    let url_entities = if check_url_overlap {
        url_entities(text)
    }
    else {
        Vec::new()
    };

    extract_hashtags(text, &url_entities)
}

fn extract_hashtags(text: &str, url_entities: &[Entity]) -> Vec<Entity> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut cursor = 0usize;

    loop {
        if cursor >= text.len() {
            break;
        }

        let substr = &text[cursor..];

        let caps = break_opt!(regexen::RE_VALID_HASHTAG.captures(substr));

        if caps.len() < 3 {
            break;
        }

        let current_cursor = cursor;
        cursor += caps.pos(0).unwrap().1;

        let hashtag_range = break_opt!(caps.pos(1));
        let text_range = break_opt!(caps.pos(2));

        //note: check character after the # to make sure it's not \u{fe0f} or \u{20e3}
        //this is because the regex crate doesn't have lookahead assertions, which the objc impl
        //used to check for this
        if regexen::RE_HASHTAG_INVALID_INITIAL_CHARS.is_match(&substr[text_range.0..text_range.1]) {
            break;
        }

        let mut match_ok = true;

        for url in url_entities {
            if (hashtag_range.0 + current_cursor) <= url.range.1 &&
                url.range.0 <= (hashtag_range.1 + current_cursor)
            {
                //this hashtag is part of a url in the same text, skip it
                match_ok = false;
                break;
            }
        }

        if match_ok {
            if regexen::RE_END_HASHTAG.is_match(&substr[hashtag_range.1..]) {
                match_ok = false;
            }
        }

        if match_ok {
            results.push(Entity {
                kind: EntityKind::Hashtag,
                range: (hashtag_range.0 + current_cursor, hashtag_range.1 + current_cursor),
            });
        }
    }

    results
}

///Parses the given string for financial symbols ("cashtags"), optionally leaving out those that
///are part of URLs.
///
///The entities returned by this function can be used to find symbols for hyperlinking, as well as
///to provide an autocompletion facility, if the byte-offset position of the cursor is known with
///relation to the full text.
///
///The `check_url_overlap` parameter behaves the same way as in `hashtag_entities`; when `true`, it
///will parse URLs from the text first and check symbols to make sure they don't overlap with any
///extracted URLs.
///
///# Example
///
///```rust
/// use egg_mode_text::symbol_entities;
///
/// let text = "some $stock symbol";
/// let mut results = symbol_entities(text, true).into_iter();
///
/// let tag = results.next().unwrap();
/// assert_eq!(tag.substr(text), "$stock");
///
/// assert_eq!(results.next(), None);
///```
pub fn symbol_entities(text: &str, check_url_overlap: bool) -> Vec<Entity> {
    if text.is_empty() {
        return Vec::new();
    }

    let url_entities = if check_url_overlap {
        url_entities(text)
    }
    else {
        Vec::new()
    };

    extract_symbols(text, &url_entities)
}

fn extract_symbols(text: &str, url_entities: &[Entity]) -> Vec<Entity> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();

    for caps in regexen::RE_VALID_SYMBOL.captures_iter(text) {
        if caps.len() < 2 { break; }

        let text_range = break_opt!(caps.pos(0));
        let symbol_range = break_opt!(caps.pos(1));
        let mut match_ok = true;

        //check the text after the match to see if it's valid; this is because i can't use
        //lookahead assertions in the regex crate and this is how it's implemented in the obj-c
        //version
        if !regexen::RE_END_SYMBOL.is_match(&text[text_range.1..]) {
            match_ok = false;
        }

        for url in url_entities {
            if symbol_range.0 <= url.range.1 && url.range.0 <= symbol_range.1 {
                //this symbol is part of a url in the same text, skip it
                match_ok = false;
                break;
            }
        }

        if match_ok {
            results.push(Entity {
                kind: EntityKind::Symbol,
                range: symbol_range,
            });
        }
    }

    results
}

///Returns how many characters the given text would be, after accounting for URL shortening.
///
///For the `http_url_len` and `https_url_len` parameters, call [`GET help/configuration`][] in the
///Twitter API (in the `egg-mode` crate, this is exposed in `egg_mode::service::config`) and use
///the `short_url_len` and `short_url_len_https` fields on the struct that's returned. If you want
///to perform these checks offline, twitter-text's sample code and tests assume 23 characters for
///both sizes. At the time of this writing (2016-11-28), those numbers were also being returned
///from the service itself.
///
///[`GET help/configuration`]: https://developer.twitter.com/en/docs/developer-utilities/configuration/api-reference/get-help-configuration
///
///# Examples
///
///```rust
/// use egg_mode_text::character_count;
///
/// let count = character_count("This is a test.", 23, 23);
/// assert_eq!(count, 15);
///
/// // URLs get replaced by a t.co URL of the given length
/// let count = character_count("test.com", 23, 23);
/// assert_eq!(count, 23);
///
/// // Multiple URLs get shortened individually
/// let count =
///     character_count("Test https://test.com test https://test.com test.com test", 23, 23);
/// assert_eq!(count, 86);
///```
pub fn character_count(text: &str, http_url_len: i32, https_url_len: i32) -> usize {
    //twitter uses code point counts after NFC normalization
    let mut text = text.nfc().collect::<String>();

    if text.is_empty() {
        return 0;
    }

    let mut url_offset = 0usize;
    let entities = url_entities(&text);

    for url in &entities {
        let substr = &text[url.range.0..url.range.1];
        if substr.contains("https") {
            url_offset += https_url_len as usize;
        }
        else {
            url_offset += http_url_len as usize;
        }
    }

    //put character removal in a second pass so we don't mess up the byte offsets
    for url in entities.iter().rev() {
        text.drain(url.range.0..url.range.1);
    }

    //make sure to count codepoints, not bytes
    let len = text.chars().count() + url_offset;

    len
}

pub fn parse_raw_lexicon(raw_lexicon: &str) -> HashMap<&str, f64> {
    let lines = raw_lexicon.split("\n");
    let mut lex_dict = HashMap::new();
    for line in lines {
        let mut split_line = line.split('\t');
        let word = split_line.next().unwrap();
        let val  = split_line.next().unwrap();
        lex_dict.insert(word, val.parse().unwrap());
    }
    lex_dict
}

pub fn parse_raw_emoji_lexicon(raw_emoji_lexicon: &str) -> HashMap<&str, &str> {
    let lines = raw_emoji_lexicon.split("\n");
    let mut emoji_dict = HashMap::new();
    for line in lines {
        let mut split_line = line.split('\t');
        let word = split_line.next().unwrap();
        let desc  = split_line.next().unwrap();
        emoji_dict.insert(word, desc);
    }
    emoji_dict
}

/**
 *  Stores glyphs and useful info about text
 **/
struct ParsedText<'a> {
    glyphs: Vec<&'a str>,
    has_mixed_caps: bool,
    punc_amplifier: f64,
}

impl<'a> ParsedText<'a> {
    //glyphizes and extracts useful properties of input text
    fn from_text(text: &'a str) -> ParsedText {
        let _glyphs = ParsedText::glyphize(text);
        let _has_mixed_caps = ParsedText::has_mixed_caps(&_glyphs);
        let _punc_amplifier = ParsedText::get_punctuation_emphasis(text);
        ParsedText {
            glyphs: _glyphs,
            has_mixed_caps: _has_mixed_caps,
            punc_amplifier: _punc_amplifier,
        }
    }

    fn glyphize(text: &'a str) -> Vec<&str> {
        let glyphs: Vec<&str> = text.split_whitespace()
            .filter(|s| s.len() > 1)
            .map(|s| ParsedText::strip_punc_if_word(s))
            .collect();
        glyphs
    }

    // Removes punctuation from words, ie "hello!!!" -> "hello" and ",don't??" -> "don't"
    // Keeps most emoticons, ie ":^)" -> ":^)"\
    fn strip_punc_if_word(glyph: &str) -> &str {
        let stripped = glyph.trim_matches(|c| PUNCTUATION.contains(c));
        if stripped.len() <= 1 {
            return glyph;
        }
        stripped
    }

    // Determines if message has a mix of both all caps and non all caps words
    fn has_mixed_caps(glyphs: &Vec<&str>) -> bool {
        let (mut has_caps, mut has_non_caps) = (false, false);
        for glyph in glyphs.iter() {
            if is_all_caps(glyph) {
                has_caps = true;
            } else {
                has_non_caps = true;
            }
            if has_non_caps && has_caps {
                return true;
            }
        }
        false
    }

    //uses empirical values to determine how the use of '?' and '!' contribute to sentiment
    fn get_punctuation_emphasis(text: &str) -> f64 {
        let emark_count: i32 = text.as_bytes().iter().filter(|b| **b == b'!').count() as i32;
        let qmark_count: i32 = text.as_bytes().iter().filter(|b| **b == b'?').count() as i32;

        let emark_emph = min(emark_count, MAX_EMARK) as f64 * EMARK_INCR;
        let mut qmark_emph = (qmark_count as f64) * QMARK_INCR;
        if qmark_count > MAX_QMARK {
            qmark_emph = MAX_QMARK_INCR;
        }
        qmark_emph + emark_emph
    }
}

//Checks if all letters in glyph are capitalized
fn is_all_caps(glyph: &str) -> bool {
    ALL_CAPS_RE.is_match(glyph) && glyph.len() > 1
}

//Checks if glyph is in the list of NEGATION_SCALAR
fn is_negated(glyph: &str) -> bool {
    if NEGATION_glyphS.contains(&glyph.to_lowercase().as_str()) {
        return true;
    }
    glyph.contains("n't")
}

//Normalizes score between -1.0 and 1.0. Alpha value is expected upper limit for a score
fn normalize_score(score: f64) -> f64 {
    let norm_score = score / (score * score + NORMALIZATION_ALPHA).sqrt();
    if norm_score < -1.0 {
        return -1.0;
    } else if norm_score > 1.0 {
        return 1.0;
    }
    norm_score
}

//Checks how previous glyphs affect the valence of the current glyph
fn scalar_inc_dec(glyph: &str, valence: f64, has_mixed_caps: bool) -> f64 {
    let mut scalar = 0.0;
    let glyph_lower: &str = &glyph.to_lowercase();
    if BOOSTER_DICT.contains_key(glyph_lower) {
        scalar = *BOOSTER_DICT.get(glyph_lower).unwrap();
        if valence < 0.0 {
            scalar *= -1.0;
        }
        if is_all_caps(glyph) && has_mixed_caps {
            if valence > 0.0 {
                scalar += C_INCR;
            } else {
                scalar -= C_INCR;
            }
        }
    }
    scalar
}

fn sum_sentiment_scores(scores: Vec<f64>) -> (f64, f64, u32) {
    let (mut pos_sum, mut neg_sum, mut neu_count) = (0f64, 0f64, 0);
    for score in scores {
        if score > 0f64 {
            pos_sum += score + 1.0;
        } else if score < 0f64 {
            neg_sum += score - 1.0;
        } else {
            neu_count += 1;
        }
    }
    (pos_sum, neg_sum, neu_count)
}

pub struct SentimentIntensityAnalyzer<'a> {
    lexicon: &'a HashMap<&'a str, f64>,
    emoji_lexicon: &'a HashMap<&'a str, &'a str>,
}

impl<'a> SentimentIntensityAnalyzer<'a> {
    pub fn new() -> SentimentIntensityAnalyzer<'static>{
        SentimentIntensityAnalyzer {
            lexicon: &LEXICON,
            emoji_lexicon: &EMOJI_LEXICON,
        }
    }

    pub fn from_lexicon<'b>(_lexicon: &'b HashMap<&str, f64>) ->
    SentimentIntensityAnalyzer<'b> {
        SentimentIntensityAnalyzer {
            lexicon: _lexicon,
            emoji_lexicon: &EMOJI_LEXICON,
        }
    }

    fn get_total_sentiment(&self, sentiments: Vec<f64>, punct_emph_amplifier: f64) -> HashMap<&str, f64> {
        let (mut neg, mut neu, mut pos, mut compound) = (0f64, 0f64, 0f64, 0f64);
        if sentiments.len() > 0 {
            let mut total_sentiment: f64 = sentiments.iter().sum();
            if total_sentiment > 0f64 {
                total_sentiment += punct_emph_amplifier;
            } else {
                total_sentiment -= punct_emph_amplifier;
            }
            compound = normalize_score(total_sentiment);

            let (mut pos_sum, mut neg_sum, neu_count) = sum_sentiment_scores(sentiments);

            if pos_sum > neg_sum.abs() {
                pos_sum += punct_emph_amplifier;
            } else if pos_sum < neg_sum.abs() {
                neg_sum -= punct_emph_amplifier;
            }

            let total = pos_sum + neg_sum.abs() + (neu_count as f64);
            pos = (pos_sum / total).abs();
            neg = (neg_sum / total).abs();
            neu = (neu_count as f64 / total).abs();
        }
        let sentiment_dict = hashmap!["neg" => neg,
                                      "neu" => neu,
                                      "pos" => pos,
                                      "compound" => compound];
        sentiment_dict
    }

    pub fn polarity_scores(&self, text: &str) -> HashMap<&str, f64>{
        let text = self.append_emoji_descriptions(text);
        let parsedtext = ParsedText::from_text(&text);
        println!("{:#?}", parsedtext.glyphs);
        let mut sentiments = Vec::new();
        let glyphs = &parsedtext.glyphs;

        for (i, word) in glyphs.iter().enumerate() {
            if BOOSTER_DICT.contains_key(word.to_lowercase().as_str()) {
                sentiments.push(0f64);
            } else if i < glyphs.len() - 1 && word.to_lowercase() == "kind"
                && glyphs[i + 1].to_lowercase() == "of" {
                sentiments.push(0f64);
            } else {
                sentiments.push(self.sentiment_valence(&parsedtext, word, i));
            }
        }
        but_check(&glyphs, &mut sentiments);
        self.get_total_sentiment(sentiments, parsedtext.punc_amplifier)
    }

    //Removes emoji and appends their description to the end the input text
    fn append_emoji_descriptions(&self, text: &str) -> String {
        let mut result = String::new();
        let mut prev_space = true;
        for chr in text.chars() {
            if self.emoji_lexicon.contains_key(chr.to_string().as_str()) {
                if !prev_space {
                    result.push(' ');
                }
                result.push_str(self.emoji_lexicon.get(&chr.to_string().as_str()).unwrap());
                prev_space = false;
            } else {
                prev_space = chr == ' ';
                result.push(chr);
            }
        }
        println!("{}", result);
        result
    }

    fn sentiment_valence(&self, parsed: &ParsedText, word: &str, i: usize) -> f64 {
        let mut valence = 0f64;
        let word_lower = word.to_lowercase();
        let glyphs = &parsed.glyphs;
        if self.lexicon.contains_key(word_lower.as_str()) {
            valence = *self.lexicon.get(word_lower.as_str()).unwrap();
            if is_all_caps(word) && parsed.has_mixed_caps {
                if valence > 0f64 {
                    valence += C_INCR;
                } else {
                    valence -= C_INCR
                }
            }
            for start_i in 0..3 {
                if i > start_i && !self.lexicon.contains_key(
                    glyphs[i - start_i - 1].to_lowercase().as_str()) {
                    let mut s = scalar_inc_dec(glyphs[i - start_i - 1], valence, parsed.has_mixed_caps);
                    if start_i == 1 {
                        s *= 0.95;
                    } else if start_i == 2 {
                        s *= 0.9
                    }
                    valence += s;
                    valence = negation_check(valence, glyphs, start_i, i);
                    if start_i == 2 {
                        valence = special_idioms_check(valence, glyphs, i);
                    }
                }
            }
            valence = least_check(valence, glyphs, i);
        }
        valence
    }
}

/**
 * Check for specific patterns or glyphs, and modify sentiment as needed
 **/
fn negation_check(valence: f64, glyphs: &Vec<&str>, start_i: usize, i: usize) -> f64 {
    let mut valence = valence;
    let glyphs: Vec<String> = glyphs.iter().map(|s| s.to_lowercase()).collect();
    if start_i == 0 {
        if is_negated(&glyphs[i - start_i - 1]) {
            valence *= NEGATION_SCALAR;
        }
    } else if start_i == 1 {
        if glyphs[i - 2] == "never" &&
            (glyphs[i - 1] == "so" ||
                glyphs[i - 1] == "this") {
            valence *= 1.25
        } else if glyphs[i - 2] == "without" && glyphs[i - 1] == "doubt" {
            valence *= 1.0
        } else if is_negated(&glyphs[i - start_i - 1]) {
            valence *= NEGATION_SCALAR;
        }
    } else if start_i == 2 {
        if glyphs[i - 3] == "never" &&
            glyphs[i - 2] == "so" || glyphs[i - 2] == "this" ||
            glyphs[i - 1] == "so" || glyphs[i - 1] == "this" {
            valence *= 1.25
        } else if glyphs[i - 3] == "without" &&
            glyphs[i - 2] == "doubt" ||
            glyphs[i - 1] == "doubt" {
            valence *= 1.0;
        } else if is_negated(&glyphs[i - start_i - 1]) {
            valence *= NEGATION_SCALAR;
        }
    }
    valence
}

// If "but" is in the glyphs, scales down the sentiment of words before "but" and
// adds more emphasis to the words after
fn but_check(glyphs: &Vec<&str>, sentiments: &mut Vec<f64>) {
    match glyphs.iter().position(|&s| s.to_lowercase() == "but") {
        Some(but_index) => {
            for i in 0..sentiments.len() {
                if i < but_index {
                    sentiments[i] *= 0.5;
                } else if i > but_index {
                    sentiments[i] *= 1.5;
                }
            }
        },
        None => return,
    }
}

fn least_check(_valence: f64, glyphs: &Vec<&str>, i: usize) -> f64 {
    let mut valence = _valence;
    if i > 1 && glyphs[i - 1].to_lowercase() == "least"
        && glyphs[i - 2].to_lowercase() != "at"
        && glyphs[i - 2].to_lowercase() != "very" {
        valence *= NEGATION_SCALAR;
    } else if i > 0 && glyphs[i - 1].to_lowercase() == "least" {
        valence *= NEGATION_SCALAR;
    }
    valence
}


///Returns how many characters would remain with the given text, if the given bound were used as a
///maximum. Also returns an indicator of whether the given text is a valid length to post with that
///maximum.
///
///This function exists as a sort of convenience method to allow clients to call one uniform method
///to show a remaining character count on a tweet compose box, and to conditionally enable a
///"submit" button.
///
///For the `http_url_len` and `https_url_len` parameters, call [`GET help/configuration`][] on the
///Twitter API (in the `egg-mode` crate, this is exposed in `egg_mode::service::config`) and use
///the `short_url_len` and `short_url_len_https` fields on the struct that's returned. If you want
///to perform these checks offline, twitter-text's sample code and tests assume 23 characters for
///both sizes. At the time of this writing (2016-11-28), those numbers were also being returned
///from the service itself.
///
///If you're writing text for a direct message and want to know how many characters are available
///in that context, see [`GET help/configuration`][] in the Twitter API (in the `egg-mode` crate,
///this is exposed in `egg_mode::service::config`) and the `dm_text_character_limit` returned by
///that endpoint, then call [`character_count`][] and subtract the result from the configuration
///value.
///
///[`GET help/configuration`]: https://developer.twitter.com/en/docs/developer-utilities/configuration/api-reference/get-help-configuration
///[`character_count`]: fn.character_count.html
///
///# Examples
///
///```rust
/// use egg_mode_text::characters_remaining;
///
/// let (count, _) = characters_remaining("This is a test.", 280, 23, 23);
/// assert_eq!(count, 280 - 15);
///
/// // URLs get replaced by a t.co URL of the given length
/// let (count, _) = characters_remaining("test.com", 280, 23, 23);
/// assert_eq!(count, 280 - 23);
///
/// // Multiple URLs get shortened individually
/// let (count, _) =
///     characters_remaining("Test https://test.com test https://test.com test.com test",
///                          280, 23, 23);
/// assert_eq!(count, 280 - 86);
///```
pub fn characters_remaining(text: &str,
                            max: usize,
                            http_url_len: i32,
                            https_url_len: i32)
    -> (usize, bool)
{
    let len = character_count(text, http_url_len, https_url_len);

    (max - len, len > 0 && len <= max)
}



#[derive(Clone, Debug)]
pub struct StreamBuilder<'a, T = Glyph> {
    method: RequestMethod,
    endpoint: Uri,
    glyph: T,
    inner: BuilderInner<'a>,
}

/// A future returned by constructor methods
/// which resolves to a `TwitterStream`.
pub struct FutureTwitterStream {
    response: MaybeTimeout<ResponseFuture>,
}

/// A listener for Twitter Streaming API.
/// It yields JSON strings returned from the API.
pub struct TwitterStream {
    inner: Lines<MaybeBrotli<MaybeTimeoutStream<Body>>>,
}

#[derive(Clone, Debug, oauth::Authorize)]
struct BuilderInner<'a> {
    #[oauth1(skip)]
    #[cfg(feature = "runtime")]
    timeout: Option<Duration>,
    #[oauth1(skip_if = "not")]
    stall_warnings: bool,
    filter_level: Option<FilterLevel>,
    language: Option<&'a str>,
    #[oauth1(encoded, fmt = "fmt_follow")]
    follow: Option<&'a [u64]>,
    track: Option<&'a str>,
    #[oauth1(encoded, fmt = "fmt_locations")]
    #[allow(clippy::type_complexity)]
    locations: Option<&'a [((f64, f64), (f64, f64))]>,
    #[oauth1(encoded)]
    count: Option<i32>,
}

impl<'a, C, A> StreamBuilder<'a, Glyph<C, A>>
where
    C: Borrow<str>,
    A: Borrow<str>,
{
    /// Create a streamBuilder for `POST statuses/filter` endpoint.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://dev.twitter.com/streaming/reference/post/statuses/filter
    pub fn filter(glyph: Glyph<C, A>) -> Self {
        const URI: &str = "https://stream.twitter.com/1.1/statuses/filter.json";
        Self::custom(RequestMethod::POST, Uri::from_static(URI), glyph)
    }

    /// Create a streamBuilder for `GET statuses/sample` endpoint.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://dev.twitter.com/streaming/reference/get/statuses/sample
    pub fn sample(glyph: Glyph<C, A>) -> Self {
        const URI: &str = "https://stream.twitter.com/1.1/statuses/sample.json";
        Self::custom(RequestMethod::GET, Uri::from_static(URI), glyph)
    }

    /// Constructs a streamBuilder for a Stream at a custom endpoint.
    pub fn custom(method: RequestMethod, endpoint: Uri, glyph: Glyph<C, A>) -> Self {
        Self {
            method,
            endpoint,
            glyph,
            inner: BuilderInner {
                #[cfg(feature = "runtime")]
                timeout: Some(Duration::from_secs(90)),
                stall_warnings: false,
                filter_level: None,
                language: None,
                follow: None,
                track: None,
                locations: None,
                count: None,
            },
        }
    }

    /// Start listening on the Streaming API endpoint, returning a `Future` which resolves
    /// to a `Stream` yielding JSON messages from the API.
    #[cfg(feature = "tls")]
    pub fn listen(&self) -> Result<FutureTwitterStream, error::TlsError> {
        let conn = hyper_tls::HttpsConnector::new()?;
        Ok(self.listen_with_client(&Client::streamBuilder().build::<_, Body>(conn)))
    }

    /// Same as `listen` except that it uses `client` to make HTTP request to the endpoint.
    pub fn listen_with_client<Conn, B>(&self, client: &Client<Conn, B>) -> FutureTwitterStream
    where
        Conn: Connect + Sync + 'static,
        Conn::Transport: 'static,
        Conn::Future: 'static,
        B: Default + From<Vec<u8>> + Payload + Unpin + Send + 'static,
        B::Data: Send + Unpin,
    {
        let mut req = Request::streamBuilder();
        req.method(self.method.clone())
            .header(ACCEPT_ENCODING, HeaderValue::from_static("Brotli"));

        let mut oauth = oauth::StreamBuilder::new(self.glyph.client.as_ref(), oauth::HmacSha1);
        oauth.glyph(self.glyph.glyph.as_ref());
        let req = if RequestMethod::POST == self.method {
            let oauth::Request {
                authorization,
                data,
            } = oauth.post_form(&self.endpoint, &self.inner);

            req.uri(self.endpoint.clone())
                .header(AUTHORIZATION, Bytes::from(authorization))
                .header(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                )
                .header(CONTENT_LENGTH, Bytes::from(data.len().to_string()))
                .body(data.into_bytes().into())
                .unwrap()
        } else {
            let oauth::Request {
                authorization,
                data: uri,
            } = oauth.build(self.method.as_ref(), &self.endpoint, &self.inner);

            req.uri(uri)
                .header(AUTHORIZATION, Bytes::from(authorization))
                .body(B::default())
                .unwrap()
        };

        let res = client.request(req);
        FutureTwitterStream {
            #[cfg(feature = "runtime")]
            response: timeout(res, self.inner.timeout),
            #[cfg(not(feature = "runtime"))]
            response: timeout(res),
        }
    }
}

impl<'a, C, A> StreamBuilder<'a, Glyph<C, A>> {
    /// Reset the HTTP request method to be used when connecting
    /// to the server.
    pub fn method(&mut self, method: RequestMethod) -> &mut Self {
        self.method = method;
        self
    }

    /// Reset the API endpoint URI to be connected.
    pub fn endpoint(&mut self, endpoint: Uri) -> &mut Self {
        self.endpoint = endpoint;
        self
    }

    /// Reset the glyph to be used to log into Twitter.
    pub fn glyph(&mut self, glyph: Glyph<C, A>) -> &mut Self {
        self.glyph = glyph;
        self
    }

    /// Set a timeout for the stream.
    ///
    /// Passing `None` disables the timeout.
    ///
    /// Default is 90 seconds.
    #[cfg(feature = "runtime")]
    pub fn timeout(&mut self, timeout: impl Into<Option<Duration>>) -> &mut Self {
        self.inner.timeout = timeout.into();
        self
    }

    /// Set whether to receive messages when in danger of
    /// being disconnected.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#stall-warnings
    pub fn stall_warnings(&mut self, stall_warnings: bool) -> &mut Self {
        self.inner.stall_warnings = stall_warnings;
        self
    }

    /// Set the minimum `filter_level` Tweet attribute to receive.
    /// The default is `FilterLevel::None`.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#filter-level
    pub fn filter_level(&mut self, filter_level: impl Into<Option<FilterLevel>>) -> &mut Self {
        self.inner.filter_level = filter_level.into();
        self
    }

    /// Set a comma-separated language identifiers to receive Tweets
    /// written in the specified languages only.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#language
    pub fn language(&mut self, language: impl Into<Option<&'a str>>) -> &mut Self {
        self.inner.language = language.into();
        self
    }

    /// Set a list of user IDs to receive Tweets from the specified users.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#follow
    pub fn follow(&mut self, follow: impl Into<Option<&'a [u64]>>) -> &mut Self {
        self.inner.follow = follow.into();
        self
    }

    /// A comma separated list of phrases to filter Tweets by.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#track
    pub fn track(&mut self, track: impl Into<Option<&'a str>>) -> &mut Self {
        self.inner.track = track.into();
        self
    }

    /// Set a list of bounding boxes to filter Tweets by,
    /// specified by a pair of coordinates in the form of
    /// `((longitude, latitude), (longitude, latitude))` tuple.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#locations
    pub fn locations(
        &mut self,
        locations: impl Into<Option<&'a [((f64, f64), (f64, f64))]>>,
    ) -> &mut Self {
        self.inner.locations = locations.into();
        self
    }

    /// The `count` parameter.
    /// This parameter requires elevated access to use.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#count
    pub fn count(&mut self, count: impl Into<Option<i32>>) -> &mut Self {
        self.inner.count = count.into();
        self
    }
}

#[cfg(feature = "tls")]
impl TwitterStream {
    /// A shorthand for `StreamBuilder::filter().listen()`.
    pub fn filter<C, A>(glyph: Glyph<C, A>) -> Result<FutureTwitterStream, error::TlsError>
    where
        C: Borrow<str>,
        A: Borrow<str>,
    {
        StreamBuilder::filter(glyph).listen()
    }

    /// A shorthand for `StreamBuilder::sample().listen()`.
    pub fn sample<C, A>(glyph: Glyph<C, A>) -> Result<FutureTwitterStream, error::TlsError>
    where
        C: Borrow<str>,
        A: Borrow<str>,
    {
        StreamBuilder::sample(glyph).listen()
    }
}

impl Future for FutureTwitterStream {
    type Output = Result<TwitterStream, Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let res = ready!(self.response.poll_unpin(cx))?;
        let (parts, body) = res.into_parts();
        let Parts {
            status, headers, ..
        } = parts;

        if StatusCode::OK != status {
            return Poll::Ready(Err(Error::Http(status)));
        }

        let body = timeout_to_stream(&self.response, body);
        let use_brotli = headers
            .get_all(CONTENT_ENCODING)
            .iter()
            .any(|e| e == "Brotli");
        let inner = if use_brotli {
            Lines::new(Brotli::Brotli(body))
        } else {
            Lines::new(Brotli::identity(body))
        };

        Poll::Ready(Ok(TwitterStream { inner }))
    }
}

impl Stream for TwitterStream {
    type Item = Result<string::String<Bytes>, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let line = ready_some!(self.inner.poll_next_unpin(cx))?;
            if line.iter().all(|&c| is_json_whitespace(c)) {
                continue;
            }

            let line = string::String::<Bytes>::try_from(line).map_err(Error::Utf8)?;
            return Poll::Ready(Some(Ok(line)));
        }
    }
}

fn is_json_whitespace(c: u8) -> bool {
    // RFC7159 §2
    b" \t\n\r".contains(&c)
}

