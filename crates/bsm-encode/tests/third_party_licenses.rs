//! The LGPL obligation created by statically linking LAME.
//!
//! WHY THIS TEST EXISTS
//! --------------------
//! `bsm-encode` depends on `mp3lame-encoder`, which depends on `mp3lame-sys`,
//! which **vendors LAME 3.100's C source and links it statically**:
//!
//!     build.rs:58   cargo:rustc-link-lib=static=mp3lame
//!     build.rs:138  cc.compile("mp3lame")
//!
//! So every Baxters Stereo Mix binary contains LAME. LAME is LGPL ("version 2
//! ... or (at your option) any later version") and the Rust wrappers declare
//! LGPL-3.0. Baxters Stereo Mix itself is MIT.
//!
//! Static linking is the case the LGPL cares most about. Because this project
//! ships its complete source under MIT, the relinking requirement
//! (LGPL-3 section 4(d)(0)) is satisfied by construction -- anyone can rebuild
//! against a modified LAME. What is NOT satisfied by construction is the
//! *notice* obligation: the distribution must say that LAME is used, under what
//! licence, and include the licence texts.
//!
//! Until 2026-08-24 `THIRD_PARTY_LICENSES` was a single line reading
//! "FFmpeg and related DLLs may be subject to LGPL. Include third-party license
//! notices here." -- a placeholder, naming a dependency this Linux port does
//! not even use.
//!
//! A licence obligation that lives only in a person's memory is one release
//! away from being forgotten, so it is pinned here.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()      // crates/
        .parent().unwrap()      // repo root
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

#[test]
fn third_party_notices_name_lame_and_its_licence() {
    let notices = read("THIRD_PARTY_LICENSES");

    assert!(
        notices.len() > 400,
        "THIRD_PARTY_LICENSES is {} bytes -- still the placeholder?",
        notices.len()
    );
    for needle in ["LAME", "lame-3.100", "LGPL", "mp3lame-sys", "mp3lame-encoder"] {
        assert!(
            notices.contains(needle),
            "THIRD_PARTY_LICENSES does not mention {needle:?}"
        );
    }
    assert!(
        notices.contains("static"),
        "the notice must say the linkage is STATIC -- that is the fact that \
         decides which LGPL obligations apply"
    );
}

#[test]
fn the_licence_texts_are_actually_shipped() {
    // LGPL-3 is written as an addendum to GPL-3 and section 4(b) requires a copy
    // of BOTH to travel with the combined work.
    for f in ["licenses/LGPL-3.0.txt", "licenses/GPL-3.0.txt", "licenses/LAME-COPYING.txt"] {
        let text = read(f);
        assert!(text.len() > 5_000, "{f} is only {} bytes -- truncated?", text.len());
    }
    assert!(read("licenses/LGPL-3.0.txt").contains("GNU LESSER GENERAL PUBLIC LICENSE"));
    assert!(read("licenses/GPL-3.0.txt").contains("GNU GENERAL PUBLIC LICENSE"));
    assert!(read("licenses/LAME-COPYING.txt").contains("LIBRARY GENERAL PUBLIC LICENSE"));
}

#[test]
fn ffmpeg_is_explained_as_absent_rather_than_implied_as_present() {
    // The Linux port has no FFmpeg DLLs. The old placeholder named FFmpeg and
    // nothing else, implying a dependency that does not exist while omitting
    // the one that does.
    //
    // The first version of this test asserted the placeholder STRING was absent
    // -- and then failed, because the replacement document quotes that string
    // while explaining the history. Quoting what a file used to say is worth
    // keeping, so the test is what was wrong: what matters is not that the words
    // never appear, but that the file no longer STARTS as that placeholder and
    // now states FFmpeg's absence outright.
    let notices = read("THIRD_PARTY_LICENSES");

    assert!(
        !notices.trim_start().starts_with("FFmpeg and related DLLs"),
        "THIRD_PARTY_LICENSES still opens with the placeholder"
    );
    assert!(
        notices.contains("does not use FFmpeg"),
        "the notices should say plainly that FFmpeg is not a dependency"
    );
}

#[test]
fn the_mit_grant_is_complete() {
    // Every LGPL argument this project makes rests on one sentence: "Baxters
    // Stereo Mix ships its complete source under MIT, so anyone can substitute
    // a modified LAME and rebuild." That sentence is only true if LICENSE is
    // actually MIT.
    //
    // Until 2026-08-25 it was not. The file was headed "MIT License" but had
    // been truncated: the attribution clause was absent, so the grant carried
    // no conditions at all, and the warranty paragraph stopped at "EXPRESS OR
    // IMPLIED." with no disclaimer or limitation of liability. A truncated MIT
    // is not MIT -- it is an unnamed permissive grant that licence scanners do
    // not recognise and that gives a downstream user nothing to comply with.
    let licence = read("LICENSE");

    assert!(
        licence.contains("MIT License"),
        "LICENSE no longer identifies itself as MIT"
    );
    assert!(
        licence.contains(
            "The above copyright notice and this permission notice shall be included in all"
        ),
        "LICENSE is missing the MIT attribution clause -- the grant's ONLY \
         condition, and the thing that makes it MIT rather than a bare gift"
    );
    // The three clauses that make the warranty paragraph a disclaimer rather
    // than a sentence fragment.
    for needle in ["MERCHANTABILITY", "FITNESS FOR A PARTICULAR PURPOSE", "NONINFRINGEMENT"] {
        assert!(
            licence.contains(needle),
            "LICENSE's warranty disclaimer is truncated -- missing {needle:?}"
        );
    }
    assert!(
        licence.contains("IN NO EVENT SHALL THE"),
        "LICENSE is missing the limitation of liability"
    );
}

#[test]
fn the_readme_carries_the_licence_notice() {
    // LGPL-3 section 4(a) requires the notice to be given to the person
    // receiving the work. THIRD_PARTY_LICENSES satisfies that, but a file
    // nobody opens is a poor way to discharge a notice obligation: the README
    // is what a visitor actually reads, so the notice has to survive there too.
    //
    // This test does not pin any particular wording. It pins the facts that
    // must be findable without leaving the front page.
    let readme = read("README.md");

    for (needle, why) in [
        ("MIT", "the project's own licence"),
        ("LAME", "the third-party component that creates the obligation"),
        ("LGPL", "the licence that component is under"),
        ("static", "the linkage, which is what decides which clauses apply"),
        ("THIRD_PARTY_LICENSES", "the pointer to the full reasoning"),
        ("licenses/LGPL-3.0.txt", "the shipped licence texts"),
        ("lame.sourceforge.io", "the acknowledgement LAME's authors ask for"),
    ] {
        assert!(
            readme.contains(needle),
            "README.md no longer mentions {needle:?} -- {why}"
        );
    }

    // Guard against the notice being reduced to a passing mention: the README
    // must still explain that shipping a binary without source breaks the
    // relinking argument, because that is the one condition under which the
    // current compliance position stops holding.
    assert!(
        readme.contains("without") && readme.contains("relink"),
        "README.md no longer states the condition that would break the \
         static-linking compliance argument"
    );
}
