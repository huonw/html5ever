// Copyright 2014 The HTML5 for Rust Project Developers. See the
// COPYRIGHT file at the top-level directory of this distribution.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

pub use self::interface::{QuirksMode, Quirks, LimitedQuirks, NoQuirks};
pub use self::interface::TreeSink;

use tokenizer;
use tokenizer::{Doctype, Attribute, StartTag, EndTag, Tag};
use tokenizer::TokenSink;
use tokenizer::states::{RawData, RawKind, Rcdata, Rawtext, ScriptData};

use util::atom::Atom;
use util::namespace::HTML;
use util::str::{is_ascii_whitespace, Runs};

use std::default::Default;
use std::mem::replace;

mod interface;
mod states;
mod data;

#[deriving(Eq, TotalEq, Clone, Show)]
enum SplitStatus {
    NotSplit,
    Whitespace,
    NotWhitespace,
}

/// We mostly only work with these tokens. Everything else is handled
/// specially at the beginning of `process_token`.
#[deriving(Eq, TotalEq, Clone, Show)]
enum Token {
    TagToken(Tag),
    CommentToken(String),
    CharacterTokens(SplitStatus, String),
    NullCharacterToken,
    EOFToken,
}

/// Tree builder options, with an impl for Default.
#[deriving(Clone)]
pub struct TreeBuilderOpts {
    /// Is scripting enabled?
    pub scripting_enabled: bool,

    /// Is this an iframe srcdoc document?
    pub iframe_srcdoc: bool,

    /// Are we parsing a HTML fragment?
    pub fragment: bool,
}

impl Default for TreeBuilderOpts {
    fn default() -> TreeBuilderOpts {
        TreeBuilderOpts {
            scripting_enabled: true,
            iframe_srcdoc: false,
            fragment: false,
        }
    }
}

pub struct TreeBuilder<'sink, Handle, Sink> {
    /// Options controlling the behavior of the tree builder.
    opts: TreeBuilderOpts,

    /// Consumer of tree modifications.
    sink: &'sink mut Sink,

    /// Insertion mode.
    mode: states::InsertionMode,

    /// Original insertion mode, used by Text and InTableText modes.
    orig_mode: Option<states::InsertionMode>,

    /// The document node, which is created by the sink.
    doc_handle: Handle,

    /// Stack of open elements, most recently added at end.
    open_elems: Vec<Handle>,

    /// Head element pointer.
    head_elem: Option<Handle>,

    /// Next state change for the tokenizer, if any.
    next_tokenizer_state: Option<tokenizer::states::State>,

    /// Frameset-ok flag.
    frameset_ok: bool,
}

// We use guards, so we can't bind tags by move.  Instead, bind by ref
// mut and take attrs with `replace`.  This is basically fine since
// empty `Vec` doesn't allocate.
fn take_attrs(t: &mut Tag) -> Vec<Attribute> {
    replace(&mut t.attrs, vec!())
}

enum SplitKind {
    DropWhitespace,
    KeepWhitespace,
}

enum ProcessResult {
    Done,
    Split(SplitKind, String),
    Reprocess(states::InsertionMode, Token),
}

macro_rules! tag_pattern (
    ($kind:ident     $var:ident) => ( TagToken(ref     $var @ Tag { kind: $kind, ..}) );
    ($kind:ident mut $var:ident) => ( TagToken(ref mut $var @ Tag { kind: $kind, ..}) );
)

macro_rules! start ( ($($args:tt)*) => ( tag_pattern!(StartTag $($args)*) ))
macro_rules! end   ( ($($args:tt)*) => ( tag_pattern!(EndTag   $($args)*) ))

macro_rules! named ( ($t:expr, $($atom:ident)*) => (
    match_atom!($t.name { $($atom)* => true, _ => false })
))

macro_rules! kind_named ( ($kind:ident $t:expr, $($atom:ident)*) => (
    match $t {
        tag_pattern!($kind t) => named!(t, $($atom)*),
        _ => false,
    }
))

macro_rules! start_named ( ($($args:tt)*) => ( kind_named!(StartTag $($args)*) ))
macro_rules! end_named   ( ($($args:tt)*) => ( kind_named!(EndTag   $($args)*) ))

macro_rules! append_with ( ( $fun:ident, $target:expr, $($args:expr),* ) => ({
    // two steps to avoid double borrow
    let target = $target;
    self.sink.$fun(target, $($args),*);
    Done
}))

macro_rules! append_text    ( ($target:expr, $text:expr) => ( append_with!(append_text,    $target, $text) ))
macro_rules! append_comment ( ($target:expr, $text:expr) => ( append_with!(append_comment, $target, $text) ))

impl<'sink, Handle: Clone, Sink: TreeSink<Handle>> TreeBuilder<'sink, Handle, Sink> {
    pub fn new(sink: &'sink mut Sink, opts: TreeBuilderOpts) -> TreeBuilder<'sink, Handle, Sink> {
        let doc_handle = sink.get_document();
        TreeBuilder {
            opts: opts,
            sink: sink,
            mode: states::Initial,
            orig_mode: None,
            doc_handle: doc_handle,
            open_elems: vec!(),
            head_elem: None,
            next_tokenizer_state: None,
            frameset_ok: true,
        }
    }

    // Switch to `Text` insertion mode, save the old mode, and
    // switch the tokenizer to a raw-data state.
    // The latter only takes effect after the current / next
    // `process_token` of a start tag returns!
    fn parse_raw_data(&mut self, k: RawKind) {
        assert!(self.next_tokenizer_state.is_none());
        self.next_tokenizer_state = Some(RawData(k));
        self.orig_mode = Some(self.mode);
        self.mode = states::Text;
    }

    // The "appropriate place for inserting a node".
    fn target(&self) -> Handle {
        // FIXME: foster parenting, templates, other nonsense
        self.open_elems.last().expect("no current element").clone()
    }

    fn push(&mut self, elem: &Handle) {
        self.open_elems.push(elem.clone());
    }

    fn pop(&mut self) -> Handle {
        self.open_elems.pop().expect("no current element")
    }

    fn remove_from_stack(&mut self, elem: Handle) {
        let new_elems = self.open_elems.iter().filter(
            |x| !self.sink.same_elem(x, elem)).collect();
        assert!(new_elems.len() + 1 == self.open_elems.len())
        self.open_elems = new_elems;
    }

    fn reconstruct_formatting(&mut self) {
        warn!("FIXME: not implemented");
    }

    fn create_root(&mut self, attrs: Vec<Attribute>) {
        let elem = self.sink.create_element(HTML, atom!(html), attrs);
        self.push(&elem);
        self.sink.append_element(self.doc_handle.clone(), elem);
        // FIXME: application cache selection algorithm
    }

    fn create_element_impl(&mut self, push: bool, name: Atom, attrs: Vec<Attribute>) -> Handle {
        let target = self.target();
        let elem = self.sink.create_element(HTML, name, attrs);
        if push {
            self.push(&elem);
        }
        self.sink.append_element(target, elem.clone());
        // FIXME: Remove from the stack if we can't append?
        elem
    }

    fn create_element(&mut self, name: Atom, attrs: Vec<Attribute>) -> Handle {
        self.create_element_impl(true, name, attrs)
    }

    fn create_element_nopush(&mut self, name: Atom, attrs: Vec<Attribute>) -> Handle {
        self.create_element_impl(false, name, attrs)
    }

    fn create_element_for(&mut self, tag: &mut Tag) -> Handle {
        self.create_element(tag.name.clone(), take_attrs(tag))
    }

    fn process_to_completion(&mut self, mut mode: states::InsertionMode, mut token: Token) {
        // Additional tokens yet to be processed. First to be processed is on
        // the *end*, because that's where Vec supports O(1) push/pop.
        // This stays empty (and hence non-allocating) in the common case
        // where we don't split whitespace.
        let mut more_tokens = vec!();

        loop {
            match self.step(mode, token) {
                Done => {
                    token = unwrap_or_return!(more_tokens.pop(), ());
                }
                Reprocess(m, t) => {
                    mode = m;
                    token = t;
                }
                Split(k, buf) => {
                    let keep = match k {
                        KeepWhitespace => true,
                        _ => false,
                    };
                    let mut it = Runs::new(is_ascii_whitespace, buf.as_slice())
                        .filter(|&(m, _)| keep || !m)
                        .map(|(m, b)| CharacterTokens(match m {
                            true => Whitespace,
                            false => NotWhitespace,
                        }, b.to_strbuf()));

                    token = it.next().expect("Empty Runs iterator");

                    // Push additional tokens in reverse order, so the next one
                    // is first to be popped.
                    // FIXME: copy/allocate less
                    let rest: Vec<Token> = it.collect();
                    for t in rest.move_iter().rev() {
                        more_tokens.push(t);
                    }
                }
            }
        }
    }

    fn step(&mut self, mode: states::InsertionMode, mut token: Token) -> ProcessResult {
        debug!("processing {} in insertion mode {:?}", token, mode);

        match mode {
            states::Initial => match token {
                CharacterTokens(NotSplit, text) => Split(DropWhitespace, text),
                CommentToken(text) => append_comment!(self.doc_handle.clone(), text),
                token => {
                    if !self.opts.iframe_srcdoc {
                        self.sink.parse_error(format!("Bad token in Initial insertion mode: {}", token));
                        self.sink.set_quirks_mode(Quirks);
                    }
                    Reprocess(states::BeforeHtml, token)
                }
            },

            states::BeforeHtml => match token {
                CharacterTokens(NotSplit, text) => Split(DropWhitespace, text),
                CommentToken(text) => append_comment!(self.doc_handle.clone(), text),
                start!(mut t) if named!(t, html) => {
                    self.create_root(take_attrs(t));
                    Done
                }
                end!(t) if !named!(t, head body html br) => {
                    self.sink.parse_error(format!("Unexpected end tag in BeforeHtml mode: {}", t));
                    Done
                }
                token => {
                    self.create_root(vec!());
                    Reprocess(states::BeforeHead, token)
                }
            },

            states::BeforeHead => match token {
                CharacterTokens(NotSplit, text) => Split(DropWhitespace, text),
                CommentToken(text) => append_comment!(self.target(), text),
                end!(t) if !named!(t, head body html br) => {
                    self.sink.parse_error(format!("Unexpected end tag in BeforeHead mode: {}", t));
                    Done
                }
                start!(mut t) if named!(t, head) => {
                    self.head_elem = Some(self.create_element_for(t));
                    self.mode = states::InHead;
                    Done
                }
                // Do this here because we can't move out of `token` when it's borrowed.
                token if start_named!(token, html) => self.step(states::InBody, token),
                token => {
                    self.head_elem = Some(self.create_element(atom!(head), vec!()));
                    Reprocess(states::InHead, token)
                }
            },

            states::InHead => match token {
                CharacterTokens(NotSplit, text) => Split(KeepWhitespace, text),
                CharacterTokens(Whitespace, text) => append_text!(self.target(), text),
                CommentToken(text) => append_comment!(self.target(), text),
                start!(mut t) if match_atom!(t.name {
                    base basefont bgsound link meta => {
                        self.create_element_nopush(t.name.clone(), take_attrs(t));
                        /* FIXME: handle charset= and http-equiv="Content-Type"
                        if named!(t, meta) {
                            ...
                        }
                        */
                        true
                    }
                    title => {
                        self.parse_raw_data(Rcdata);
                        self.create_element_for(t);
                        true
                    }
                    noframes style noscript => {
                        if (!self.opts.scripting_enabled) && named!(t, noscript) {
                            self.create_element_for(t);
                            self.mode = states::InHeadNoscript;
                        } else {
                            self.parse_raw_data(Rawtext);
                            self.create_element_for(t);
                        }
                        true
                    }
                    script => {
                        let target = self.target();
                        let elem = self.sink.create_element(HTML, atom!(script), take_attrs(t));
                        if self.opts.fragment {
                            self.sink.mark_script_already_started(elem.clone());
                        }
                        self.push(&elem);
                        self.sink.append_element(target, elem);
                        self.parse_raw_data(ScriptData);
                        true
                    }
                    template => fail!("FIXME: <template> not implemented"),
                    head => {
                        self.sink.parse_error("<head> in insertion mode InHead".to_owned());
                        true
                    }
                    _ => false,
                }) => Done,
                end!(mut t) if match_atom!(t.name {
                    head => {
                        self.pop();
                        self.mode = states::AfterHead;
                        true
                    }
                    body html br => false,
                    template => fail!("FIXME: <template> not implemented"),
                    _ => {
                        self.sink.parse_error(format!("Unexpected end tag in InHead mode: {}", t));
                        true
                    }
                }) => Done,
                token if start_named!(token, html) => self.step(states::InBody, token),
                token {
                    self.pop();
                    Reprocess(states::AfterHead, token)
                }
            },

            states::InHeadNoscript => match token {
                CharacterTokens(NotSplit, text) => Split(KeepWhitespace, text),
                CharacterTokens(Whitespace, text) => append_text!(self.target(), text),
                end!(t) if match_atom!(t.name {
                    noscript => {
                        self.pop();
                        self.mode = states::InHead;
                        true
                    }
                    br => false,
                    _ => {
                        self.sink.parse_error(format!("Unexpected end tag in InHeadNoscript mode: {}", t));
                        true
                    }
                }) => Done,
                start!(t) if named!(t, head noscript) => {
                    self.sink.parse_error(format!("Unexpected start tag in InHeadNoscript mode: {}", t));
                    Done
                }

                token @ CommentToken(_) => self.step(states::InHead, token),

                token if start_named!(token, html)
                    => self.step(states::InBody, token),
                token if start_named!(token, basefont bgsound link meta noframes style)
                    => self.step(states::InHead, token),
                token => {
                    self.sink.parse_error(format!("Unexpected token in InHeadNoscript mode: {}", token));
                    self.pop();
                    Reprocess(states::InHead, token)
                }
            },

            states::AfterHead => match token {
                CharacterTokens(NotSplit, text) => Split(KeepWhitespace, text),
                CharacterTokens(Whitespace, text) => append_text!(self.target(), text),
                CommentToken(text) => append_comment!(self.target(), text),
                token if start_named!(token,
                        base basefont bgsound link meta noframes script style template title) => {
                    self.sink.parse_error(format!("Unexpected start tag in AfterHead mode: {}", t));
                    let head = self.head_elem.as_ref().expect("No head element").clone();
                    self.push(head);
                    let result = self.step(states::InHead, token);
                    self.remove_from_stack(head);
                    result
                }
                token if end_named!(token, template) => self.step(states::InHead, token),
                start!(t) if match_atom!(t.name {
                    html => false,
                    body => {
                        self.create_element_for(t);
                        self.frameset_ok = false;
                        self.mode = states::InBody;
                        true
                    }
                    frameset => {
                        self.create_element_for(t);
                        self.mode = states::InFrameset;
                        true
                    }
                    head => {
                        self.sink.parse_error(format!("Unexpected start tag in AfterHead mode: {}", t));
                        true
                    }
                }) => Done,
                end!(t) if !named!(t, body html br) => {
                    self.sink.parse_error(format!("Unexpected end tag in AfterHead mode: {}", t));
                    true
                }
                token => {
                    self.create_element(atom!(body), vec!());
                    Reprocess(states::InBody, token)
                }
            },

            states::InBody => match token {
                NullCharacterToken => {
                    self.sink.parse_error("Null character in InBody mode".to_string());
                    Done
                }
                CharacterTokens(_, text) => {
                    self.reconstruct_formatting();    
                    if self.frameset_ok && text.as_slice().iter().any(|c| !is_ascii_whitespace(c)) {
                        self.frameset_ok = false;
                    }
                    append_text!(self.target(), text)
                }
                CommentToken(text) => append_comment!(self.target(), text),
                token if
                    start_named!(token, base basefont bgsound link meta noframes script style template title)
                    || end_named!(token, template) => self.step(states::InHead, token),
            },

              states::Text
            | states::InTable
            | states::InTableText
            | states::InCaption
            | states::InColumnGroup
            | states::InTableBody
            | states::InRow
            | states::InCell
            | states::InSelect
            | states::InSelectInTable
            | states::InTemplate
            | states::AfterBody
            | states::InFrameset
            | states::AfterFrameset
            | states::AfterAfterBody
            | states::AfterAfterFrameset
                => fail!("not implemented"),
        }
    }
}

impl<'sink, Handle: Clone, Sink: TreeSink<Handle>> TokenSink for TreeBuilder<'sink, Handle, Sink> {
    fn process_token(&mut self, token: tokenizer::Token) {
        // Handle `ParseError` and `DoctypeToken`; convert everything else to the local `Token` type.
        let token = match token {
            tokenizer::ParseError(e) => {
                self.sink.parse_error(e);
                return;
            }

            tokenizer::DoctypeToken(dt) => if self.mode == states::Initial {
                let (err, quirk) = data::doctype_error_and_quirks(&dt, self.opts.iframe_srcdoc);
                if err {
                    self.sink.parse_error(format!("Bad DOCTYPE: {}", dt));
                }
                let Doctype { name, public_id, system_id, force_quirks: _ } = dt;
                self.sink.append_doctype_to_document(
                    name.unwrap_or(String::new()),
                    public_id.unwrap_or(String::new()),
                    system_id.unwrap_or(String::new())
                );
                self.sink.set_quirks_mode(quirk);

                self.mode = states::BeforeHtml;
                return;
            } else {
                self.sink.parse_error(format!("DOCTYPE in insertion mode {:?}", self.mode));
                return;
            },

            tokenizer::TagToken(x) => TagToken(x),
            tokenizer::CommentToken(x) => CommentToken(x),
            tokenizer::CharacterTokens(x) => CharacterTokens(NotSplit, x),
            tokenizer::NullCharacterToken => NullCharacterToken,
            tokenizer::EOFToken => EOFToken,
        };

        self.process_to_completion(self.mode, token);
    }

    fn query_state_change(&mut self) -> Option<tokenizer::states::State> {
        self.next_tokenizer_state.take()
    }
}
