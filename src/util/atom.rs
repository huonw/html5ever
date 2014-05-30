// Copyright 2014 The HTML5 for Rust Project Developers. See the
// COPYRIGHT file at the top-level directory of this distribution.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use phf::PhfMap;

use std::mem::replace;

static static_atom_map: PhfMap<uint> = static_atom_map!();
static static_atom_array: &'static [&'static str] = static_atom_array!();

// Assume that a string which can be interned always is.
// FIXME: Revisit this assumption when we have dynamic interning.
/// Interned string.
#[deriving(Clone, Show, Eq, TotalEq)]
pub enum Atom {
    Static(uint),
    // dynamic interning goes here
    Owned(String),
}

impl Atom {
    pub fn from_str(s: &str) -> Atom {
        match static_atom_map.find(&s) {
            Some(&k) => Static(k),
            None => Owned(s.to_string()),
        }
    }

    pub fn from_buf(s: String) -> Atom {
        match static_atom_map.find(&s.as_slice()) {
            Some(&k) => Static(k),
            None => Owned(s),
        }
    }

    /// Like `Atom::from_buf(replace(s, String::new()))` but avoids
    /// allocating a new `String` when the string is interned --
    /// just truncates the old one.
    pub fn take_from_buf(s: &mut String) -> Atom {
        match static_atom_map.find(&s.as_slice()) {
            Some(&k) => {
                s.truncate(0);
                Static(k)
            }
            None => {
                Owned(replace(s, String::new()))
            }
        }
    }

    /// Only for use by the atom!() macro!
    #[inline(always)]
    #[experimental="Only for use by the atom!() macro"]
    pub fn unchecked_static_atom_from_macro(i: uint) -> Atom {
        Static(i)
    }

    #[inline(always)]
    pub fn get_static_atom_id_from_macro(&self) -> Option<uint> {
        match *self {
            Static(i) => Some(i),
            _ => None,
        }
    }

    #[inline(always)]
    fn fast_partial_eq(&self, other: &Atom) -> Option<bool> {
        match (self, other) {
            (&Static(x), &Static(y)) => Some(x == y),
            _ => None,
        }
    }
}

fn get_static(i: uint) -> &'static str {
    *static_atom_array.get(i).expect("bad static atom")
}

impl Str for Atom {
    fn as_slice<'t>(&'t self) -> &'t str {
        match *self {
            Static(i) => get_static(i),
            Owned(ref s) => s.as_slice(),
        }
    }
}

impl StrAllocating for Atom {
    fn into_string(self) -> String {
        match self {
            Static(i) => get_static(i).to_string(),
            Owned(s) => s.into_string(),
        }
    }

    fn to_string(&self) -> String {
        match *self {
            Static(i) => get_static(i).to_string(),
            Owned(ref s) => s.clone(),
        }
    }

    fn into_owned(self) -> String {
        match self {
            Static(i) => get_static(i).to_string(),
            Owned(s) => s,
        }
    }
}

impl Ord for Atom {
    fn lt(&self, other: &Atom) -> bool {
        match self.fast_partial_eq(other) {
            Some(true) => false,
            _ => self.as_slice() < other.as_slice(),
        }
    }
}

impl TotalOrd for Atom {
    fn cmp(&self, other: &Atom) -> Ordering {
        match self.fast_partial_eq(other) {
            Some(true) => Equal,
            _ => self.as_slice().cmp(&other.as_slice()),
        }
    }
}

#[test]
fn interned() {
    match Atom::from_str("body") {
        Static(i) => assert_eq!(get_static(i), "body"),
        _ => fail!("wrong interning"),
    }
}

#[test]
fn not_interned() {
    match Atom::from_str("asdfghjk") {
        Owned(b) => assert_eq!(b.as_slice(), "asdfghjk"),
        _ => fail!("wrong interning"),
    }
}

#[test]
fn as_slice() {
    assert_eq!(Atom::from_str("").as_slice(), "");
    assert_eq!(Atom::from_str("body").as_slice(), "body");
    assert_eq!(Atom::from_str("asdfghjk").as_slice(), "asdfghjk");
}

#[test]
fn into_string() {
    assert_eq!(Atom::from_str("").into_string(), "".to_string());
    assert_eq!(Atom::from_str("body").into_string(), "body".to_string());
    assert_eq!(Atom::from_str("asdfghjk").into_string(), "asdfghjk".to_string());
}

#[test]
fn to_string() {
    assert_eq!(Atom::from_str("").to_string(), "".to_string());
    assert_eq!(Atom::from_str("body").to_string(), "body".to_string());
    assert_eq!(Atom::from_str("asdfghjk").to_string(), "asdfghjk".to_string());
}

#[test]
fn into_string() {
    assert_eq!(Atom::from_str("").into_string(), "".to_string());
    assert_eq!(Atom::from_str("body").into_string(), "body".to_string());
    assert_eq!(Atom::from_str("asdfghjk").into_string(), "asdfghjk".to_string());
}

#[test]
fn take_from_buf_interned() {
    let mut b = "body".to_string();
    let a = Atom::take_from_buf(&mut b);
    assert_eq!(a, Atom::from_str("body"));
    assert_eq!(b, String::new());
}

#[test]
fn take_from_buf_not_interned() {
    let mut b = "asdfghjk".to_string();
    let a = Atom::take_from_buf(&mut b);
    assert_eq!(a, Atom::from_str("asdfghjk"));
    assert_eq!(b, String::new());
}

#[test]
fn ord() {
    fn check(x: &str, y: &str) {
        assert_eq!(x < y, Atom::from_str(x) < Atom::from_str(y));
        assert_eq!(x.cmp(&y), Atom::from_str(x).cmp(&Atom::from_str(y)));
    }

    check("a", "body");
    check("asdf", "body");
    check("zasdf", "body");
    check("z", "body");

    check("a", "bbbbb");
    check("asdf", "bbbbb");
    check("zasdf", "bbbbb");
    check("z", "bbbbb");
}

#[test]
fn atom_macro() {
    assert_eq!(atom!(body), Atom::from_str("body"));
    assert_eq!(atom!("body"), Atom::from_str("body"));
    assert_eq!(atom!("font-weight"), Atom::from_str("font-weight"));
}

#[test]
fn match_atom() {
    assert_eq!(2, match_atom!(Atom::from_str("head") {
        br => 1,
        html head => { 2 }
        _ => 3,
    }));

    assert_eq!(3, match_atom!(Atom::from_str("body") {
        br => { 1 }
        html head => 2,
        _ => { 3 }
    }));

    assert_eq!(3, match_atom!(Atom::from_str("zzzzzz") {
        br => 1,
        html head => 2,
        _ => 3,
    }));
}
