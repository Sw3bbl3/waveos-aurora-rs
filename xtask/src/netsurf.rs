//! NetSurf, the web browser, for `/System/NetSurf`.
//!
//! NetSurf 3.11 (GPL-2.0, a separate program) is built from its full
//! source release with its framebuffer front end and a WaveOS surface for
//! libnsfb (`third_party/netsurf/waveos`), so it draws straight into a
//! WaveOS window, like Doom. Its libraries come from Debian 13 (the
//! upstream sources with Debian's patches): curl with mbedTLS for HTTP and
//! HTTPS, zlib, libpng, libjpeg-turbo, FreeType and expat. Every download
//! is pinned by SHA-256.
//!
//! NetSurf's own build runs Perl, gperf and helper programs it compiles to
//! generate some sources; those generators are done here instead (see
//! [`generate`]), so building needs only the C compiler for Linux programs
//! ([`Cc`]) and the usual `tar` and `patch`.

use super::linux::{fetch, unpack_deb, Cc};
use super::{root, Program};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

const NETSURF: (&str, &str) = (
    "https://download.netsurf-browser.org/netsurf/releases/source-full/netsurf-all-3.11.tar.gz",
    "4dea880ff3c2f698bfd62c982b259340f9abcd7f67e6c8eb2b32c61f71644b7b",
);

/// mbed TLS's options, for it and the code using its headers: big-number
/// division without 128-bit division, which the Clang build (linked
/// without a compiler runtime) can't call.
const TLS_CONFIG: &[&str] = &["-DMBEDTLS_NO_UDBL_DIVISION"];

const SNAPSHOT: &str = "https://snapshot.debian.org/archive/debian/20261001T000000Z";

/// A library's upstream source from Debian, Debian's patches, and the
/// patches left out (documentation only, for files Debian's repack drops).
struct Upstream {
    name: &'static str,
    orig: (&'static str, &'static str),
    debian: (&'static str, &'static str),
    skip: &'static [&'static str],
}

const LIBRARIES: &[Upstream] = &[
    Upstream {
        name: "zlib",
        orig: (
            "pool/main/z/zlib/zlib_1.3.dfsg+really1.3.1.orig.tar.gz",
            "60dd315c07f616887caa029408308a018ace66e3d142726a97db164b3b8f69fb",
        ),
        debian: (
            "pool/main/z/zlib/zlib_1.3.dfsg+really1.3.1-1.debian.tar.xz",
            "9ed525955ce9fb0c1b39be8ff98f73450dbfc6305a9a27e6149c8972d38a0a9e",
        ),
        skip: &[],
    },
    Upstream {
        name: "libpng",
        orig: (
            "pool/main/libp/libpng1.6/libpng1.6_1.6.48.orig.tar.gz",
            "b17e99026055727e8cba99160c3a9a7f9af788e9f786daeadded5a42243f1dd0",
        ),
        debian: (
            "pool/main/libp/libpng1.6/libpng1.6_1.6.48-1+deb13u5.debian.tar.xz",
            "b38d5929b9a6139f2ce02a0e831d0f3f5365410b050dc429ea079a73253fe97a",
        ),
        skip: &[],
    },
    Upstream {
        name: "libjpeg-turbo",
        orig: (
            "pool/main/libj/libjpeg-turbo/libjpeg-turbo_2.1.5.orig.tar.gz",
            "254f3642b04e309fee775123133c6464181addc150499561020312ec61c1bf7c",
        ),
        debian: (
            "pool/main/libj/libjpeg-turbo/libjpeg-turbo_2.1.5-4.debian.tar.xz",
            "739e7dc22904dccdc5ab105de57a6e4c1515c0e841e68226e6410ff4976e0e91",
        ),
        skip: &[],
    },
    Upstream {
        name: "freetype",
        orig: (
            "pool/main/f/freetype/freetype_2.13.3+dfsg.orig.tar.xz",
            "686ec73cbf6783b245dd068a09ce807b729ac0f8a46dd70f7867923c32fdf4de",
        ),
        debian: (
            "pool/main/f/freetype/freetype_2.13.3+dfsg-1+deb13u1.debian.tar.xz",
            "e7a07cad1b9c803457ec310bc87132429f52171c89f9126c09eb62e203c8c84b",
        ),
        skip: &["no-web-fonts.patch", "hide-donations-information.patch"],
    },
    Upstream {
        name: "mbedtls",
        orig: (
            "pool/main/m/mbedtls/mbedtls_3.6.6.orig.tar.bz2",
            "8fb65fae8dcae5840f793c0a334860a411f884cc537ea290ce1c52bb64ca007a",
        ),
        debian: (
            "pool/main/m/mbedtls/mbedtls_3.6.6-0.1~deb13u1.debian.tar.xz",
            "210466c522eeebc85cf2f76bd5aef935b4ebac465559cc0a84f9e73a8ed6601c",
        ),
        skip: &[],
    },
    Upstream {
        name: "curl",
        orig: (
            "pool/main/c/curl/curl_8.14.1.orig.tar.gz",
            "6766ada7101d292b42b8b15681120acd68effa4a9660935853cf6d61f0d984d4",
        ),
        debian: (
            "pool/main/c/curl/curl_8.14.1-2+deb13u5.debian.tar.xz",
            "ebdfcfab1c2001c7a539fe2a4044dbf666a544393f191bedce9ee2d07c4ea925",
        ),
        skip: &[],
    },
    Upstream {
        name: "expat",
        orig: (
            "pool/main/e/expat/expat_2.8.3.orig.tar.gz",
            "533659a16e0184035a99fd8e783f1ad61a887a7bf8586a8681740b9d7ed42389",
        ),
        debian: (
            "pool/main/e/expat/expat_2.8.3-1~deb13u1.debian.tar.xz",
            "4bf2c5c2f189673c66f79f2f8a8727cd8c08636d46b648c896e1c73673562c7b",
        ),
        skip: &[],
    },
];

/// Debian packages whose files go into `/System/NetSurf`.
const DEBS: &[(&str, &str)] = &[
    (
        "pool/main/c/ca-certificates/ca-certificates_20250419_all.deb",
        "ef590f89563aa4b46c8260d49d1cea0fc1b181d19e8df3782694706adf05c184",
    ),
    (
        "pool/main/f/fonts-dejavu/fonts-dejavu-core_2.37-8_all.deb",
        "86635b3d25b3655fc11cb3ecc3af59f0bf19643b02b94f2de48bd10253cdba12",
    ),
    (
        "pool/main/f/fonts-dejavu/fonts-dejavu-mono_2.37-8_all.deb",
        "3003e98a5debfdeadc7040a7f715fe9fe6fb67f68deacf6049b54e30f07fc014",
    ),
];

/// What NetSurf's build system defines for every library (GCC and clang).
const ALIGNED: &str = "-D_ALIGNED=__attribute__((aligned))";

/// Where NetSurf finds its resources and fonts in WaveOS.
const HOME: &str = "/System/NetSurf";

fn work() -> PathBuf {
    root().join("target/netsurf")
}

fn here() -> PathBuf {
    root().join("third_party/netsurf")
}

/// Unpacks `archive` into `dir` (its single top folder becomes `dir`),
/// once; true if it was unpacked now.
fn unpack_tar(archive: &Path, dir: &Path) -> bool {
    if dir.join(".unpacked").exists() {
        return false;
    }
    let tmp = dir.with_extension("tmp");
    let _ = fs::remove_dir_all(&tmp);
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(&tmp).unwrap();
    let status = Command::new("tar").arg("-xf").arg(archive).arg("-C").arg(&tmp).status().expect("run tar");
    assert!(status.success(), "could not unpack {}", archive.display());
    let entries: Vec<PathBuf> = fs::read_dir(&tmp).unwrap().flatten().map(|e| e.path()).collect();
    match entries.as_slice() {
        [only] if only.is_dir() => {
            fs::rename(only, dir).unwrap();
            fs::remove_dir(&tmp).unwrap();
        }
        _ => fs::rename(&tmp, dir).unwrap(),
    }
    true
}

/// Applies the patch `file` in `dir`.
fn apply_patch(dir: &Path, file: &Path) -> bool {
    Command::new("patch")
        .arg("-d")
        .arg(dir)
        .args(["-p1", "-N", "-t", "-s", "-i"])
        .arg(file)
        .status()
        .is_ok_and(|s| s.success())
}

/// A library's source, with Debian's patches applied.
fn library(lib: &Upstream) -> Option<PathBuf> {
    let dir = work().join("src").join(lib.name);
    let orig = fetch(&format!("{SNAPSHOT}/{}", lib.orig.0), lib.orig.1)?;
    if unpack_tar(&orig, &dir) {
        let debian = fetch(&format!("{SNAPSHOT}/{}", lib.debian.0), lib.debian.1)?;
        let status = Command::new("tar").arg("-xf").arg(&debian).arg("-C").arg(&dir).status().expect("run tar");
        assert!(status.success(), "could not unpack {}", debian.display());
        let patches = dir.join("debian/patches");
        let series = fs::read_to_string(patches.join("series")).unwrap_or_default();
        for p in series.lines().map(|l| l.split_whitespace().next().unwrap_or("")) {
            if p.is_empty() || p.starts_with('#') || lib.skip.contains(&p) {
                continue;
            }
            assert!(apply_patch(&dir, &patches.join(p)), "Debian patch {p} doesn't apply to {}", lib.name);
        }
        fs::write(dir.join(".unpacked"), "").unwrap();
    }
    Some(dir)
}

/// NetSurf's source, with WaveOS's patches and the generated sources.
fn netsurf_source() -> Option<PathBuf> {
    let dir = work().join("src/netsurf-all");
    let tarball = fetch(NETSURF.0, NETSURF.1)?;
    let mut patches: Vec<PathBuf> = fs::read_dir(here().join("patches"))
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "patch")).collect())
        .unwrap_or_default();
    patches.sort();
    // The stamp holds the patches applied; changed patches mean a fresh copy.
    let applied: String = patches.iter().map(|p| fs::read_to_string(p).unwrap()).collect();
    let stamp = dir.join(".unpacked");
    if fs::read_to_string(&stamp).is_ok_and(|s| s != applied) {
        fs::remove_file(&stamp).unwrap();
    }
    if unpack_tar(&tarball, &dir) {
        for p in &patches {
            assert!(apply_patch(&dir, p), "{} doesn't apply to NetSurf", p.display());
        }
        fs::write(&stamp, applied).unwrap();
    }
    generate(&dir);
    Some(dir)
}

// ----------------------------------------------------------------- building

/// One C file to compile.
struct Unit {
    src: PathBuf,
    obj: PathBuf,
    flags: std::rc::Rc<Vec<OsString>>,
}

/// The C files of one library: `sources` (relative to `dir`) with `flags`.
fn units(name: &str, dir: &Path, sources: &[String], flags: Vec<OsString>, out: &mut Vec<Unit>) {
    let flags = std::rc::Rc::new(flags);
    for s in sources {
        let obj = work().join("obj").join(name).join(s.replace('/', "_")).with_extension("o");
        out.push(Unit { src: dir.join(s), obj, flags: flags.clone() });
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Compiles every unit not already compiled since `since`, on every CPU.
fn compile_all(cc: &Cc, units: &[Unit], since: SystemTime) {
    struct Job<'a> {
        src: &'a Path,
        obj: &'a Path,
        flags: Vec<&'a OsStr>,
    }
    let jobs: Vec<Job> = units
        .iter()
        .filter(|u| !mtime(&u.obj).is_some_and(|t| t >= since && mtime(&u.src).is_some_and(|s| s <= t)))
        .map(|u| Job { src: &u.src, obj: &u.obj, flags: u.flags.iter().map(OsString::as_os_str).collect() })
        .collect();
    if jobs.is_empty() {
        return;
    }
    println!("NetSurf: compiling {} of {} C files", jobs.len(), units.len());
    let next = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(job) = jobs.get(i) else { break };
                fs::create_dir_all(job.obj.parent().unwrap()).unwrap();
                if !cc.compile(job.src, job.obj, &job.flags) {
                    eprintln!("error: could not compile {}", job.src.display());
                    failed.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    let failed = failed.into_inner();
    assert!(failed == 0, "{failed} NetSurf sources did not compile");
}

fn flags(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

fn include(dirs: &[&Path]) -> Vec<OsString> {
    dirs.iter().map(|d| OsString::from(format!("-I{}", d.display()))).collect()
}

fn names(list: &str) -> Vec<String> {
    list.split_whitespace().map(String::from).collect()
}

/// Every `.c` file in `dir` (relative path `rel`), sorted.
fn c_files(dir: &Path, rel: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir.join(rel))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".c"))
                .map(|n| if rel.is_empty() { n } else { format!("{rel}/{n}") })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Builds NetSurf (`target/netsurf/netsurf`) and returns what goes into
/// `/System/NetSurf`, or `None` (with a warning) if a download failed.
pub fn build(cc: &Cc) -> Option<Vec<Program>> {
    let mut src = BTreeMap::new();
    for lib in LIBRARIES {
        src.insert(lib.name, library(lib)?);
    }
    let ns = netsurf_source()?;
    let cfg = here().join("config");
    let gen = work().join("gen");
    fs::create_dir_all(&gen).unwrap();

    let (z, png, jpeg, ft, tls, curl, expat) = (
        &src["zlib"],
        &src["libpng"],
        &src["libjpeg-turbo"],
        &src["freetype"],
        &src["mbedtls"],
        &src["curl"],
        &src["expat"],
    );
    // Headers the libraries expect from their own build systems.
    fs::copy(png.join("scripts/pnglibconf.h.prebuilt"), gen.join("pnglibconf.h")).unwrap();
    let jversion = fs::read_to_string(jpeg.join("jversion.h.in")).unwrap().replace("@COPYRIGHT_YEAR@", "1991-2023");
    fs::write(gen.join("jversion.h"), jversion).unwrap();
    // libdom's parser bindings, where its users look for them.
    for b in ["hubbub", "xml"] {
        let to = gen.join("dom/bindings").join(b);
        fs::create_dir_all(&to).unwrap();
        for h in fs::read_dir(ns.join("libdom/bindings").join(b)).unwrap().flatten() {
            if h.path().extension().is_some_and(|x| x == "h") {
                fs::copy(h.path(), to.join(h.file_name())).unwrap();
            }
        }
    }

    let mut all = Vec::new();
    // Warnings are only shown when a file fails, but what newer compilers
    // refuse (and what truncates pointers on 64-bit) fails everywhere.
    let quiet =
        flags(&["-DNDEBUG", "-Werror=implicit-function-declaration", "-Werror=implicit-int", "-Werror=int-conversion"]);
    let with =
        |base: &[OsString], extra: Vec<OsString>| -> Vec<OsString> { base.iter().cloned().chain(extra).collect() };

    units(
        "zlib",
        z,
        &names("adler32.c compress.c crc32.c deflate.c gzclose.c gzlib.c gzread.c gzwrite.c infback.c inffast.c inflate.c inftrees.c trees.c uncompr.c zutil.c"),
        with(&quiet, flags(&["-DZ_HAVE_UNISTD_H", "-D_LARGEFILE64_SOURCE=1"])),
        &mut all,
    );
    units(
        "png",
        png,
        &names("png.c pngerror.c pngget.c pngmem.c pngpread.c pngread.c pngrio.c pngrtran.c pngrutil.c pngset.c pngtrans.c pngwio.c pngwrite.c pngwtran.c pngwutil.c"),
        with(&quiet, include(&[&gen, png, z])),
        &mut all,
    );
    units(
        "jpeg",
        jpeg,
        &names("jcapimin.c jcapistd.c jccoefct.c jccolor.c jcdctmgr.c jchuff.c jcicc.c jcinit.c jcmainct.c jcmarker.c jcmaster.c jcomapi.c jcparam.c jcphuff.c jcprepct.c jcsample.c jctrans.c jdapimin.c jdapistd.c jdatadst.c jdatasrc.c jdcoefct.c jdcolor.c jddctmgr.c jdhuff.c jdicc.c jdinput.c jdmainct.c jdmarker.c jdmaster.c jdmerge.c jdphuff.c jdpostct.c jdsample.c jdtrans.c jerror.c jfdctflt.c jfdctfst.c jfdctint.c jidctflt.c jidctfst.c jidctint.c jidctred.c jquant1.c jquant2.c jutils.c jmemmgr.c jmemnobs.c jaricom.c jcarith.c jdarith.c jsimd_none.c"),
        with(&quiet, include(&[&cfg, &gen, jpeg])),
        &mut all,
    );
    let ft_flags = with(
        &quiet,
        [
            flags(&["-DFT2_BUILD_LIBRARY", "-DFT_CONFIG_MODULES_H=<ftmodule.h>", "-DFT_CONFIG_OPTION_SYSTEM_ZLIB"]),
            include(&[&cfg, &ft.join("include"), z]),
        ]
        .concat(),
    );
    units(
        "freetype",
        ft,
        &names("src/base/ftsystem.c src/base/ftinit.c src/base/ftdebug.c src/base/ftbase.c src/base/ftbbox.c src/base/ftbitmap.c src/base/ftglyph.c src/base/ftsynth.c src/base/ftmm.c src/autofit/autofit.c src/truetype/truetype.c src/cff/cff.c src/psaux/psaux.c src/psnames/psnames.c src/pshinter/pshinter.c src/sfnt/sfnt.c src/smooth/smooth.c src/raster/raster.c src/gzip/ftgzip.c src/cache/ftcache.c"),
        ft_flags,
        &mut all,
    );
    units(
        "mbedtls",
        tls,
        &c_files(tls, "library"),
        with(&quiet, [flags(TLS_CONFIG), include(&[&tls.join("include"), &tls.join("library")])].concat()),
        &mut all,
    );
    let mut curl_src = c_files(curl, "lib");
    for sub in ["lib/vauth", "lib/vtls", "lib/vquic", "lib/vssh", "lib/curlx"] {
        curl_src.extend(c_files(curl, sub));
    }
    units(
        "curl",
        curl,
        &curl_src,
        with(
            &quiet,
            [
                flags(&["-DHAVE_CONFIG_H", "-DBUILDING_LIBCURL", "-DCURL_STATICLIB", "-include", "curl_config.h"]),
                flags(TLS_CONFIG),
                include(&[&cfg, &curl.join("include"), &curl.join("lib"), &tls.join("include"), z]),
            ]
            .concat(),
        ),
        &mut all,
    );
    units(
        "expat",
        expat,
        &names("expat/lib/xmlparse.c expat/lib/xmlrole.c expat/lib/xmltok.c expat/lib/random_getrandom.c expat/lib/random_dev_urandom.c"),
        with(&quiet, [flags(&["-DHAVE_EXPAT_CONFIG_H"]), include(&[&cfg, &expat.join("expat/lib")])].concat()),
        &mut all,
    );

    // NetSurf's libraries.
    let inc = |lib: &str| ns.join(lib).join("include");
    let nslib = |lib: &str, extra: &[&Path]| -> Vec<OsString> {
        let mut f = with(
            &quiet,
            flags(&[
                "-std=c99",
                "-D_BSD_SOURCE",
                "-D_DEFAULT_SOURCE",
                "-D_POSIX_C_SOURCE=200809L",
                "-D_XOPEN_SOURCE=700",
                ALIGNED,
                "-DSTMTEXPR=1",
            ]),
        );
        f.extend(include(&[&inc(lib), &ns.join(lib).join("src")]));
        f.extend(include(extra));
        f
    };
    let (wap, pu, hub, dom, css) =
        (inc("libwapcaplet"), inc("libparserutils"), inc("libhubbub"), inc("libdom"), inc("libcss"));
    units("wapcaplet", &ns.join("libwapcaplet"), &names("src/libwapcaplet.c"), nslib("libwapcaplet", &[]), &mut all);
    units(
        "parserutils",
        &ns.join("libparserutils"),
        &names("src/input/filter.c src/input/inputstream.c src/charset/aliases.c src/charset/codec.c src/charset/codecs/codec_ascii.c src/charset/codecs/codec_8859.c src/charset/codecs/codec_ext8.c src/charset/codecs/codec_utf8.c src/charset/codecs/codec_utf16.c src/charset/encodings/utf8.c src/charset/encodings/utf16.c src/utils/buffer.c src/utils/errors.c src/utils/stack.c src/utils/vector.c"),
        [nslib("libparserutils", &[]), flags(&["-DWITHOUT_ICONV_FILTER"])].concat(),
        &mut all,
    );
    let mut hub_src = names("src/parser.c src/tokeniser/entities.c src/tokeniser/tokeniser.c src/charset/detect.c src/utils/errors.c src/utils/string.c");
    hub_src
        .extend(c_files(&ns.join("libhubbub"), "src/treebuilder").into_iter().filter(|f| !f.contains("autogenerated")));
    units("hubbub", &ns.join("libhubbub"), &hub_src, nslib("libhubbub", &[&pu]), &mut all);
    let mut dom_src = Vec::new();
    for d in ["src/core", "src/events", "src/html", "src/utils"] {
        dom_src.extend(c_files(&ns.join("libdom"), d));
    }
    dom_src.extend(names("bindings/hubbub/parser.c bindings/xml/expat_xmlparser.c"));
    units(
        "dom",
        &ns.join("libdom"),
        &dom_src,
        nslib("libdom", &[&ns.join("libdom"), &pu, &wap, &hub, &gen, &expat.join("expat/lib")]),
        &mut all,
    );
    let mut css_src = names("src/stylesheet.c src/lex/lex.c");
    for d in ["src/select", "src/select/properties", "src/parse", "src/parse/properties", "src/charset", "src/utils"] {
        css_src
            .extend(c_files(&ns.join("libcss"), d).into_iter().filter(|f| !f.ends_with("css_property_parser_gen.c")));
    }
    units("css", &ns.join("libcss"), &css_src, nslib("libcss", &[&pu, &wap]), &mut all);
    units("nsbmp", &ns.join("libnsbmp"), &names("src/libnsbmp.c"), nslib("libnsbmp", &[]), &mut all);
    units("nsgif", &ns.join("libnsgif"), &names("src/gif.c src/lzw.c"), nslib("libnsgif", &[]), &mut all);
    units(
        "svgtiny",
        &ns.join("libsvgtiny"),
        &names("src/svgtiny.c src/svgtiny_gradient.c src/svgtiny_list.c"),
        nslib("libsvgtiny", &[&dom, &wap, &gen]),
        &mut all,
    );
    units(
        "nsutils",
        &ns.join("libnsutils"),
        &names("src/base64.c src/time.c src/unistd.c"),
        nslib("libnsutils", &[]),
        &mut all,
    );
    units(
        "utf8proc",
        &ns.join("libutf8proc"),
        &names("src/utf8proc.c"),
        nslib("libutf8proc", &[&inc("libutf8proc").join("libutf8proc")]),
        &mut all,
    );
    let nsfb_src = names("src/libnsfb.c src/dump.c src/cursor.c src/palette.c src/plot/api.c src/plot/util.c src/plot/generic.c src/plot/32bpp-xrgb8888.c src/plot/32bpp-xbgr8888.c src/plot/16bpp.c src/plot/8bpp.c src/surface/surface.c src/surface/ram.c");
    units("nsfb", &ns.join("libnsfb"), &nsfb_src, nslib("libnsfb", &[]), &mut all);
    units(
        "nsfb-waveos",
        &here().join("waveos"),
        &names("surface.c"),
        nslib("libnsfb", &[&root().join("libs/linux/include")]),
        &mut all,
    );

    // NetSurf itself, with its framebuffer front end.
    let nsd = ns.join("netsurf");
    let mut nsflags = with(
        &quiet,
        flags(&[
            "-std=c99",
            "-Dnsframebuffer",
            "-Dsmall",
            "-D_POSIX_C_SOURCE=200809L",
            "-D_XOPEN_SOURCE=700",
            "-D_BSD_SOURCE",
            "-D_DEFAULT_SOURCE",
            "-D_NETBSD_SOURCE",
            "-DWITH_CURL",
            "-DWITH_PNG",
            "-DWITH_JPEG",
            "-DWITH_BMP",
            "-DWITH_GIF",
            "-DWITH_NS_SVG",
            "-DWITH_UTF8PROC",
            "-DLIBICONV_PLUG",
            "-DSTMTEXPR=1",
            ALIGNED,
            "-DFB_USE_FREETYPE",
            "-DNETSURF_UA_FORMAT_STRING=\"Mozilla/5.0 (%s) NetSurf/%d.%d\"",
            "-DNETSURF_HOMEPAGE=\"about:welcome\"",
            "-DNETSURF_LOG_LEVEL=INFO",
            "-DNETSURF_BUILTIN_LOG_FILTER=\"(level:WARNING)\"",
            "-DNETSURF_BUILTIN_VERBOSE_FILTER=\"(level:VERBOSE)\"",
        ]),
    );
    nsflags.push(format!("-DNETSURF_FB_RESPATH=\"{HOME}/res\"").into());
    nsflags.push(format!("-DNETSURF_FB_FONTPATH=\"{HOME}/fonts\"").into());
    for (k, v) in [
        ("SANS_SERIF", "DejaVuSans.ttf"),
        ("SANS_SERIF_BOLD", "DejaVuSans-Bold.ttf"),
        ("SANS_SERIF_ITALIC", "DejaVuSans.ttf"),
        ("SANS_SERIF_ITALIC_BOLD", "DejaVuSans-Bold.ttf"),
        ("SERIF", "DejaVuSerif.ttf"),
        ("SERIF_BOLD", "DejaVuSerif-Bold.ttf"),
        ("MONOSPACE", "DejaVuSansMono.ttf"),
        ("MONOSPACE_BOLD", "DejaVuSansMono-Bold.ttf"),
        ("CURSIVE", "DejaVuSans.ttf"),
        ("FANTASY", "DejaVuSans.ttf"),
    ] {
        nsflags.push(format!("-DNETSURF_FB_FONT_{k}=\"{v}\"").into());
    }
    nsflags.extend(include(&[
        &nsd,
        &nsd.join("include"),
        &nsd.join("frontends"),
        &nsd.join("content/handlers"),
        &cfg,
        &gen,
        &wap,
        &pu,
        &hub,
        &dom,
        &css,
        &inc("libnsbmp"),
        &inc("libnsgif"),
        &inc("libsvgtiny"),
        &inc("libnsutils"),
        &inc("libutf8proc"),
        &inc("libnsfb"),
        &curl.join("include"),
        png,
        jpeg,
        &ft.join("include"),
        z,
        &root().join("libs/linux/include"),
    ]));
    let mut ns_src: Vec<String> = Vec::new();
    let add = |v: &mut Vec<String>, dir: &str, list: &str| {
        v.extend(list.split_whitespace().map(|f| if dir.is_empty() { f.to_string() } else { format!("{dir}/{f}") }));
    };
    add(
        &mut ns_src,
        "content",
        "content.c content_factory.c fetch.c hlcache.c llcache.c mimesniff.c textsearch.c urldb.c no_backing_store.c",
    );
    add(&mut ns_src, "content/fetchers", "data.c resource.c curl.c");
    add(&mut ns_src, "content/fetchers/about", "about.c blank.c certificate.c chart.c choices.c config.c imagecache.c nscolours.c query.c query_auth.c query_fetcherror.c query_privacy.c query_timeout.c testament.c");
    add(&mut ns_src, "content/fetchers/file", "dirlist.c file.c");
    add(&mut ns_src, "content/handlers/image", "image.c image_cache.c bmp.c gif.c ico.c jpeg.c png.c svg.c");
    add(&mut ns_src, "content/handlers/css", "css.c dump.c internal.c hints.c select.c");
    add(&mut ns_src, "content/handlers/javascript", "fetcher.c none/none.c");
    add(&mut ns_src, "content/handlers/html", "box_construct.c box_inspect.c box_manipulate.c box_normalise.c box_special.c box_textarea.c css.c css_fetcher.c dom_event.c font.c form.c forms.c html.c imagemap.c interaction.c layout.c layout_flex.c object.c redraw.c redraw_border.c script.c table.c textselection.c");
    add(&mut ns_src, "content/handlers/text", "textplain.c");
    add(&mut ns_src, "utils", "bloom.c corestrings.c file.c filename.c filepath.c hashmap.c hashtable.c idna.c libdom.c log.c messages.c nscolour.c nsoption.c punycode.c ssl_certs.c talloc.c time.c url.c useragent.c utf8.c utils.c");
    add(&mut ns_src, "utils/http", "challenge.c generics.c primitives.c parameter.c cache-control.c content-disposition.c content-type.c strict-transport-security.c www-authenticate.c");
    add(&mut ns_src, "utils/nsurl", "nsurl.c parse.c");
    add(&mut ns_src, "desktop", "cookie_manager.c knockout.c hotlist.c mouse.c plot_style.c print.c search.c searchweb.c scrollbar.c textarea.c version.c system_colour.c local_history.c global_history.c treeview.c page-info.c bitmap.c browser.c browser_window.c browser_history.c download.c frames.c netsurf.c cw_helper.c save_complete.c save_text.c selection.c textinput.c gui_factory.c save_pdf.c font_haru.c");
    add(&mut ns_src, "frontends/framebuffer", "gui.c framebuffer.c schedule.c bitmap.c fetch.c findfile.c corewindow.c local_history.c clipboard.c font_freetype.c fbtk/fbtk.c fbtk/event.c fbtk/fill.c fbtk/bitmap.c fbtk/user.c fbtk/window.c fbtk/text.c fbtk/scroll.c fbtk/osk.c");
    for (name, _) in IMAGES {
        ns_src.push(format!("frontends/framebuffer/image-{name}.c"));
    }
    units("netsurf-waveos", &here().join("waveos"), &names("netsurf.c"), nsflags.clone(), &mut all);
    units("netsurf", &nsd, &ns_src, nsflags, &mut all);

    // Anything that changed the recipe means compiling again.
    let mut since = mtime(&root().join("xtask/src/netsurf.rs")).unwrap();
    for d in [here().join("config"), here().join("waveos")] {
        for e in fs::read_dir(d).unwrap().flatten() {
            since = since.max(mtime(&e.path()).unwrap());
        }
    }
    for lib in src.values().chain([&ns]) {
        since = since.max(mtime(&lib.join(".unpacked")).unwrap());
    }
    compile_all(cc, &all, since);
    let exe = work().join("netsurf");
    let objs: Vec<PathBuf> = all.iter().map(|u| u.obj.clone()).collect();
    if !mtime(&exe).is_some_and(|t| objs.iter().all(|o| mtime(o).is_some_and(|m| m <= t))) {
        println!("NetSurf: linking");
        assert!(cc.link(&exe, &objs), "could not link NetSurf");
    }
    Some(files(&ns, exe))
}

/// What goes into `/System/NetSurf` (and the licenses).
fn files(ns: &Path, exe: PathBuf) -> Vec<Program> {
    let mut out = vec![(String::from("NetSurf/netsurf"), exe)];
    let res = ns.join("netsurf/frontends/framebuffer/res");
    let resources = ns.join("netsurf/resources");
    for (f, from) in [
        ("adblock.css", res.join("adblock.css")),
        ("default.css", res.join("default.css")),
        ("internal.css", res.join("internal.css")),
        ("quirks.css", res.join("quirks.css")),
        ("netsurf.png", res.join("netsurf.png")),
        ("favicon.png", res.join("favicon.png")),
        ("credits.html", resources.join("en/credits.html")),
        ("licence.html", resources.join("en/licence.html")),
        ("welcome.html", resources.join("en/welcome.html")),
        ("Messages", work().join("gen/Messages")),
    ] {
        out.push((format!("NetSurf/res/{f}"), from));
    }
    let mut certs = Vec::new();
    for (path, sha) in DEBS {
        let Some(dir) = fetch(&format!("{SNAPSHOT}/{path}"), sha).and_then(|d| unpack_deb(&d)) else { continue };
        let fonts = dir.join("usr/share/fonts/truetype/dejavu");
        for e in fs::read_dir(&fonts).into_iter().flatten().flatten() {
            out.push((format!("NetSurf/fonts/{}", e.file_name().to_string_lossy()), e.path()));
        }
        // Each package's copyright file (the fonts' license, the certificates').
        for doc in fs::read_dir(dir.join("usr/share/doc")).into_iter().flatten().flatten() {
            let package = doc.file_name().to_string_lossy().into_owned();
            out.push((format!("Licenses/netsurf-{package}.txt"), doc.path().join("copyright")));
        }
        let mozilla = dir.join("usr/share/ca-certificates/mozilla");
        let mut crts: Vec<PathBuf> = fs::read_dir(&mozilla).into_iter().flatten().flatten().map(|e| e.path()).collect();
        crts.sort();
        certs.extend(crts);
    }
    let bundle = work().join("ca-bundle.pem");
    // A network that inspects TLS needs its own root: WAVEOS_EXTRA_CA names a PEM file to add.
    if let Some(extra) = std::env::var_os("WAVEOS_EXTRA_CA") {
        println!("NetSurf: adding the certificates in {}", Path::new(&extra).display());
        certs.push(PathBuf::from(extra));
    }
    let pem: String = certs.iter().map(|c| fs::read_to_string(c).unwrap() + "\n").collect();
    fs::write(&bundle, pem).unwrap();
    out.push((String::from("NetSurf/ca-bundle.pem"), bundle));
    out.push((String::from("Licenses/netsurf.txt"), ns.join("netsurf/COPYING")));
    // NetSurf's own libraries (MIT) that are built in.
    for lib in [
        "libcss",
        "libdom",
        "libhubbub",
        "libnsbmp",
        "libnsfb",
        "libnsgif",
        "libnsutils",
        "libparserutils",
        "libsvgtiny",
        "libwapcaplet",
    ] {
        out.push((format!("Licenses/netsurf-{lib}.txt"), ns.join(lib).join("COPYING")));
    }
    out.push((String::from("Licenses/netsurf-libutf8proc.txt"), ns.join("libutf8proc/LICENSE.md")));
    for (lib, file) in [
        ("curl", "COPYING"),
        ("mbedtls", "LICENSE"),
        ("libpng", "LICENSE"),
        ("libjpeg-turbo", "LICENSE.md"),
        ("freetype", "LICENSE.TXT"),
        ("zlib", "LICENSE"),
        ("expat", "COPYING"),
    ] {
        out.push((format!("Licenses/netsurf-{lib}.txt"), work().join("src").join(lib).join(file)));
    }
    out
}

// --------------------------------------------------------------- generators

/// The framebuffer front end's built-in pictures (toolbar, pointers,
/// throbber), from `res/` PNG files to C.
const IMAGES: &[(&str, &str)] = &[
    ("left_arrow", "icons/back.png"),
    ("right_arrow", "icons/forward.png"),
    ("reload", "icons/reload.png"),
    ("stop_image", "icons/stop.png"),
    ("history_image", "icons/history.png"),
    ("left_arrow_g", "icons/back_g.png"),
    ("right_arrow_g", "icons/forward_g.png"),
    ("reload_g", "icons/reload_g.png"),
    ("stop_image_g", "icons/stop_g.png"),
    ("history_image_g", "icons/history_g.png"),
    ("scrolll", "icons/scrolll.png"),
    ("scrollr", "icons/scrollr.png"),
    ("scrollu", "icons/scrollu.png"),
    ("scrolld", "icons/scrolld.png"),
    ("osk_image", "icons/osk.png"),
    ("pointer_image", "pointers/default.png"),
    ("hand_image", "pointers/point.png"),
    ("caret_image", "pointers/caret.png"),
    ("menu_image", "pointers/menu.png"),
    ("progress_image", "pointers/progress.png"),
    ("move_image", "pointers/move.png"),
    ("throbber0", "throbber/throbber0.png"),
    ("throbber1", "throbber/throbber1.png"),
    ("throbber2", "throbber/throbber2.png"),
    ("throbber3", "throbber/throbber3.png"),
    ("throbber4", "throbber/throbber4.png"),
    ("throbber5", "throbber/throbber5.png"),
    ("throbber6", "throbber/throbber6.png"),
    ("throbber7", "throbber/throbber7.png"),
    ("throbber8", "throbber/throbber8.png"),
];

/// Writes the sources NetSurf's build would generate.
fn generate(ns: &Path) {
    let read = |p: &str| fs::read_to_string(ns.join(p)).unwrap();
    let write = |p: &str, s: String| write_if_changed(&ns.join(p), &s);
    write("libparserutils/src/charset/aliases.inc", gen_aliases(&read("libparserutils/build/Aliases")));
    write("libhubbub/src/tokeniser/entities.inc", gen_entities(&read("libhubbub/build/Entities")));
    write(
        "libhubbub/src/treebuilder/autogenerated-element-type.c",
        gen_lookup(&read("libhubbub/src/treebuilder/element-type.gperf"), false),
    );
    write("libsvgtiny/src/autogenerated_colors.c", gen_lookup(&read("libsvgtiny/src/colors.gperf"), true));
    for line in read("libcss/src/parse/properties/properties.gen").lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, _)) = line.split_once(':') else { continue };
        write(&format!("libcss/src/parse/properties/autogenerated_{name}.c"), gen_css_parser(line));
    }
    let res = ns.join("netsurf/frontends/framebuffer/res");
    for (name, png) in IMAGES {
        write(&format!("netsurf/frontends/framebuffer/image-{name}.c"), gen_image(&res.join(png), name));
    }
    fs::create_dir_all(work().join("gen")).unwrap();
    write_if_changed(&work().join("gen/Messages"), &gen_messages(&read("netsurf/resources/FatMessages"), "en", "fb"));
}

/// Writes `text` to `path` unless it already holds it (so nothing
/// depending on it compiles again).
fn write_if_changed(path: &Path, text: &str) {
    if fs::read_to_string(path).ok().as_deref() != Some(text) {
        fs::write(path, text).unwrap();
    }
}

/// libparserutils' character set aliases (`build/make-aliases.pl`).
fn gen_aliases(text: &str) -> String {
    const UNICODE: &[&str] = &["ISO-10646-UCS-2", "ISO-10646-UCS-4", "UTF-8"];
    let mut charsets: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for line in text.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let mut f = line.split_whitespace();
        let (Some(canon), Some(mib)) = (f.next(), f.next()) else { continue };
        charsets.insert(canon.to_string(), (mib.to_string(), f.map(String::from).collect()));
    }
    let norm = |s: &str| s.to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>();
    let mut out = String::from(
        "/*\n * This file is part of LibParserUtils.\n * Licensed under the MIT License,\n *                http://www.opensource.org/licenses/mit-license.php\n * Copyright 2010 The NetSurf Project.\n *\n * Note: This file is automatically generated by make-aliases.pl\n *\n * Do not edit file file, changes will be overwritten during build.\n */\n\nstatic parserutils_charset_aliases_canon canonical_charset_names[] = {\n",
    );
    let mut aliases: BTreeMap<String, usize> = BTreeMap::new();
    let mut unicode = Vec::new();
    for (n, (canon, (mib, others))) in charsets.iter().enumerate() {
        out.push_str(&format!("\t{{ {mib}, {}, \"{canon}\" }},\n", canon.len()));
        if UNICODE.contains(&canon.as_str()) || canon.starts_with("UTF-16") || canon.starts_with("UTF-32") {
            unicode.push(format!("((x) == {mib})"));
        }
        aliases.insert(norm(canon), n);
        for a in others {
            aliases.insert(norm(a), n);
        }
    }
    out.push_str(&format!(
        "}};\n\nstatic const uint16_t charset_aliases_canon_count = {};\n\ntypedef struct {{\n\tuint16_t name_len;\n\tconst char *name;\n\tparserutils_charset_aliases_canon *canon;\n}} parserutils_charset_aliases_alias;\n\nstatic parserutils_charset_aliases_alias charset_aliases[] = {{\n",
        charsets.len()
    ));
    for (alias, n) in &aliases {
        out.push_str(&format!("\t{{ {}, \"{alias}\", &canonical_charset_names[{n}] }},\n", alias.len()));
    }
    out.push_str(&format!(
        "}};\n\nstatic const uint16_t charset_aliases_count = {};\n\n#define MIBENUM_IS_UNICODE(x) ({})\n",
        aliases.len(),
        unicode.join(" || ")
    ));
    out
}

/// Hubbub's entity table, a ternary search tree (`build/make-entities.pl`).
fn gen_entities(text: &str) -> String {
    #[derive(Default)]
    struct Node {
        split: u8,
        lt: Option<usize>,
        eq: Option<usize>,
        gt: Option<usize>,
        value: Option<String>,
    }
    fn insert(nodes: &mut Vec<Node>, at: Option<usize>, key: &[u8], code: &str) -> usize {
        let pivot = key.first().copied().unwrap_or(0);
        let tail = if key.is_empty() { &key[..0] } else { &key[1..] };
        let i = at.unwrap_or_else(|| {
            nodes.push(Node { split: pivot, ..Default::default() });
            nodes.len() - 1
        });
        let split = nodes[i].split;
        if pivot < split {
            let n = insert(nodes, nodes[i].lt, key, code);
            nodes[i].lt = Some(n);
        } else if pivot == split {
            if pivot == 0 {
                nodes[i].value = Some(code.to_string());
            } else {
                if tail.is_empty() {
                    nodes[i].value = Some(code.to_string());
                }
                let n = insert(nodes, nodes[i].eq, tail, code);
                nodes[i].eq = Some(n);
            }
        } else {
            let n = insert(nodes, nodes[i].gt, key, code);
            nodes[i].gt = Some(n);
        }
        i
    }
    let mut entities: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let mut f = line.split_whitespace();
        if let (Some(e), Some(c)) = (f.next(), f.next()) {
            entities.insert(e.to_string(), c.to_string());
        }
    }
    // Inserting the middle key first, then each half, keeps the tree shallow.
    let keys: Vec<(&String, &String)> = entities.iter().collect();
    let mut order = Vec::new();
    let mut spans = vec![(0, keys.len())];
    while let Some((a, b)) = spans.pop() {
        if a < b {
            let m = (a + b) / 2;
            order.push(m);
            spans.push((m + 1, b));
            spans.push((a, m));
        }
    }
    let mut nodes = Vec::new();
    let mut root = None;
    for i in order {
        root = Some(insert(&mut nodes, root, keys[i].0.as_bytes(), keys[i].1));
    }
    let idx = |o: Option<usize>| o.map_or(String::from("-1"), |i| i.to_string());
    let mut out = String::from(
        "/*\n * This file is part of Hubbub.\n * Licensed under the MIT License,\n *                http://www.opensource.org/licenses/mit-license.php\n * Copyright 2010 The NetSurf Project.\n *\n * Note: This file is automatically generated by make-entities.pl\n *\n * Do not edit file file, changes will be overwritten during build.\n */\n\n\nstatic hubbub_entity_node dict[] = {\n",
    );
    for n in &nodes {
        out.push_str(&format!(
            "\t{{ {}, {}, {}, {}, {} }},\n",
            n.split,
            idx(n.lt),
            idx(n.eq),
            idx(n.gt),
            n.value.as_deref().unwrap_or("0")
        ));
    }
    out.push_str(&format!("}};\n\nstatic int32_t dict_root = {};\n", root.unwrap_or(0)));
    out
}

/// What gperf makes of a keyword file: the `%{ %}` code, a table and its
/// lookup function (a binary search here, not a perfect hash).
fn gen_lookup(text: &str, make_static: bool) -> String {
    let mut code = String::new();
    let mut lookup = String::new();
    let mut ignore_case = false;
    let mut strukt = String::new();
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut section = 0;
    let mut in_code = false;
    for line in text.lines() {
        if in_code {
            if line.trim() == "%}" {
                in_code = false;
            } else {
                code.push_str(line);
                code.push('\n');
            }
            continue;
        }
        if line.trim() == "%{" {
            in_code = true;
            continue;
        }
        if line.trim() == "%%" {
            section += 1;
            continue;
        }
        if section == 0 {
            if let Some(n) = line.strip_prefix("%define lookup-function-name ") {
                lookup = n.trim().to_string();
            } else if line.trim() == "%ignore-case" {
                ignore_case = true;
            } else if let Some(s) = line.strip_prefix("struct ") {
                strukt = s.trim_end_matches(';').trim().to_string();
            }
        } else if section == 1 && !line.trim().is_empty() {
            let (k, v) = line.split_once(',').expect("keyword line");
            rows.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let lower = |s: &str| s.to_ascii_lowercase();
    if ignore_case {
        rows.sort_by_key(|r| lower(&r.0));
    } else {
        rows.sort_by(|a, b| a.0.cmp(&b.0));
    }
    let mut out = format!("/* Generated from a gperf keyword list by WaveOS's xtask. */\n{code}\n");
    out.push_str(&format!("static const struct {strukt} wordlist[] = {{\n"));
    for (k, v) in &rows {
        out.push_str(&format!("\t{{ \"{k}\", {v} }},\n"));
    }
    let cmp = if ignore_case { "strncasecmp" } else { "strncmp" };
    out.push_str(&format!(
        "}};\n\n{}const struct {strukt} *\n{lookup}(register const char *str, register size_t len)\n{{\n\tsize_t lo = 0, hi = sizeof(wordlist) / sizeof(wordlist[0]);\n\twhile (lo < hi) {{\n\t\tsize_t mid = (lo + hi) / 2;\n\t\tconst char *name = wordlist[mid].name;\n\t\tsize_t n = strlen(name);\n\t\tint c = {cmp}(str, name, len < n ? len : n);\n\t\tif (c == 0)\n\t\t\tc = (len > n) - (len < n);\n\t\tif (c == 0)\n\t\t\treturn &wordlist[mid];\n\t\tif (c < 0)\n\t\t\thi = mid;\n\t\telse\n\t\t\tlo = mid + 1;\n\t}}\n\treturn NULL;\n}}\n",
        if make_static { "static " } else { "" }
    ));
    if ignore_case {
        out.insert_str(0, "#include <strings.h>\n");
    }
    out
}

/// One of libcss's property parsers, from its line in `properties.gen`
/// (`css_property_parser_gen.c`).
fn gen_css_parser(descriptor: &str) -> String {
    #[derive(Clone)]
    struct Kv {
        key: String,
        val: String,
    }
    let mut base: Vec<Kv> = Vec::new();
    let (mut ident, mut ident_list, mut length_unit, mut uri, mut wrap, mut number, mut color) =
        (vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
    #[derive(Clone, Copy, PartialEq)]
    enum Section {
        Base,
        Ident,
        IdentList,
        LengthUnit,
        Number,
    }
    let mut cur = Section::Base;
    let (mut do_token_check, mut only_ident, mut is_generic) = (true, true, false);
    for item in descriptor.split(' ').filter(|s| !s.is_empty()) {
        let (key, val) = match item.split_once(':') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (item.to_string(), String::new()),
        };
        let kv = Kv { key: key.clone(), val: val.clone() };
        match key.as_str() {
            "WRAP" => {
                wrap.push(kv);
                only_ident = false;
            }
            "NUMBER" => {
                if val.starts_with('(') {
                    cur = Section::Number;
                } else if val.starts_with(')') {
                    cur = Section::Base;
                } else {
                    number.push(kv);
                }
                only_ident = false;
            }
            "IDENT" => {
                if val.starts_with('(') {
                    cur = Section::Ident;
                } else if val.starts_with(')') {
                    cur = Section::Base;
                } else if ["INHERIT", "INITIAL", "REVERT", "UNSET"].contains(&val.as_str()) {
                    ident.push(Kv { key: val.clone(), val: String::new() });
                }
            }
            "IDENT_LIST" => {
                if val.starts_with('(') {
                    cur = Section::IdentList;
                } else if val.starts_with(')') {
                    cur = Section::Base;
                }
            }
            "LENGTH_UNIT" => {
                if val.starts_with('(') {
                    cur = Section::LengthUnit;
                } else if val.starts_with(')') {
                    cur = Section::Base;
                }
                only_ident = false;
                do_token_check = false;
            }
            "COLOR" => {
                color.push(kv);
                do_token_check = false;
                only_ident = false;
            }
            "URI" => {
                uri.push(kv);
                only_ident = false;
            }
            "GENERIC" => is_generic = true,
            _ => match cur {
                Section::Base => base.push(kv),
                Section::Ident => ident.push(kv),
                Section::IdentList => ident_list.push(kv),
                Section::LengthUnit => length_unit.push(kv),
                Section::Number => number.push(kv),
            },
        }
    }
    assert!(base.len() == 1, "bad libcss property descriptor: {descriptor}");
    let id = &base[0];
    let mut o = format!(
        "/*\n * This file was generated by LibCSS gen_parser \n * \n * Generated from:\n *\n * {descriptor}\n * \n * Licensed under the MIT License,\n *\t\t  http://www.opensource.org/licenses/mit-license.php\n * Copyright 2010 The NetSurf Browser Project.\n */\n\n#include <assert.h>\n#include <string.h>\n\n#include \"bytecode/bytecode.h\"\n#include \"bytecode/opcodes.h\"\n#include \"parse/properties/properties.h\"\n#include \"parse/properties/utils.h\"\n\n/**\n * Parse {}\n *\n * \\param c\t  Parsing context\n * \\param vector  Vector of tokens to process\n * \\param ctx\t  Pointer to vector iteration context\n * \\param result  resulting style\n{} * \\return CSS_OK on success,\n *\t   CSS_NOMEM on memory exhaustion,\n *\t   CSS_INVALID if the input is not valid\n *\n * Post condition: \\a *ctx is updated with the next token to process\n *\t\t   If the input is invalid, then \\a *ctx remains unchanged.\n */\ncss_error css__parse_{}(css_language *c,\n\t\tconst parserutils_vector *vector, int32_t *ctx,\n\t\tcss_style *result{})\n{{\n",
        id.key,
        if is_generic { " * \\param op\t Bytecode OpCode for CSS property to encode\n" } else { "" },
        id.key,
        if is_generic { ", enum css_properties_e op" } else { "" },
    );
    if let Some(w) = wrap.first() {
        o.push_str(&format!("\treturn {}(c, vector, ctx, result, {});\n}}\n", w.val, id.val));
        return o;
    }
    // The token type check.
    o.push_str(&format!(
        "\tint32_t orig_ctx = *ctx;\n\tcss_error error;\n\tconst css_token *token;\n\tbool match;\n\n\ttoken = parserutils_vector_iterate(vector, ctx);\n\tif ({}token == NULL{}",
        if do_token_check { "(" } else { "" },
        if do_token_check { ")" } else { "" }
    ));
    if do_token_check {
        let mut checks = Vec::new();
        if !ident.is_empty() {
            checks.push("(token->type != CSS_TOKEN_IDENT)");
        }
        if !uri.is_empty() {
            checks.push("(token->type != CSS_TOKEN_URI)");
        }
        if !number.is_empty() {
            checks.push("(token->type != CSS_TOKEN_NUMBER)");
        }
        o.push_str(&format!(" || ({})", checks.join(" && ")));
    }
    o.push_str(") {\n\t\t*ctx = orig_ctx;\n\t\treturn CSS_INVALID;\n\t}\n\n\t");
    for k in &ident {
        o.push_str("if (");
        if !only_ident {
            o.push_str("(token->type == CSS_TOKEN_IDENT) &&\n\t\t\t");
        }
        o.push_str(&format!(
            "(lwc_string_caseless_isequal(\n\t\t\ttoken->idata, c->strings[{}],\n\t\t\t&match) == lwc_error_ok && match)) {{\n",
            k.key
        ));
        match k.key.as_str() {
            "INHERIT" => {
                o.push_str(&format!("\t\terror = css_stylesheet_style_inherit(result,\n\t\t\t\t{});\n\n", id.val))
            }
            "INITIAL" => {
                o.push_str(&format!("\t\terror = css_stylesheet_style_initial(result,\n\t\t\t\t{});\n\n", id.val))
            }
            "REVERT" => {
                o.push_str(&format!("\t\terror = css_stylesheet_style_revert(result,\n\t\t\t\t{});\n\n", id.val))
            }
            "UNSET" => o.push_str(&format!("\t\terror = css_stylesheet_style_unset(result,\n\t\t\t\t{});\n\n", id.val)),
            _ => o.push_str(&format!(
                "\t\terror = css__stylesheet_style_appendOPV(result,\n\t\t\t\t{},\n\t\t\t\t{});\n\n",
                id.val, k.val
            )),
        }
        o.push_str("\t} else ");
    }
    if let Some(k) = uri.first() {
        o.push_str(&format!(
            "if (token->type == CSS_TOKEN_URI) {{\n\t\tlwc_string *uri = NULL;\n\t\tuint32_t uri_snumber;\n\n\t\terror = c->sheet->resolve(c->sheet->resolve_pw,\n\t\t\t\tc->sheet->url,\n\t\t\t\ttoken->idata, &uri);\n\t\tif (error != CSS_OK) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn error;\n\t\t}}\n\n\t\terror = css__stylesheet_string_add(c->sheet, uri, &uri_snumber);\n\t\tif (error != CSS_OK) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn error;\n\t\t}}\n\n\t\terror = css__stylesheet_style_appendOPV(result, {}, 0, {});\n\t\tif (error != CSS_OK) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn error;\n\t\t}}\n\n\t\terror = css__stylesheet_style_append(result, uri_snumber);\n\t}} else ",
            id.val, k.val
        ));
    }
    if let Some(k) = number.first() {
        o.push_str(&format!(
            "if (token->type == CSS_TOKEN_NUMBER) {{\n\t\tcss_fixed num = 0;\n\t\tsize_t consumed = 0;\n\n\t\tnum = css__number_from_lwc_string(token->idata, {}, &consumed);\n\t\t/* Invalid if there are trailing characters */\n\t\tif (consumed != lwc_string_length(token->idata)) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn CSS_INVALID;\n\t\t}}\n",
            k.key
        ));
        for r in number.iter().skip(1).filter(|r| r.key == "RANGE") {
            o.push_str(&format!(
                "\t\tif ({}) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn CSS_INVALID;\n\t\t}}\n\n",
                r.val
            ));
        }
        o.push_str(&format!(
            "\t\terror = css__stylesheet_style_appendOPV(result, {}, 0, {});\n\t\tif (error != CSS_OK) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn error;\n\t\t}}\n\n\t\terror = css__stylesheet_style_append(result, num);\n\t}} else ",
            id.val, k.val
        ));
    }
    if !color.is_empty() {
        o.push_str(&format!(
            "{{\n\t\tuint16_t value = 0;\n\t\tuint32_t color = 0;\n\t\t*ctx = orig_ctx;\n\n\t\terror = css__parse_colour_specifier(c, vector, ctx, &value, &color);\n\t\tif (error != CSS_OK) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn error;\n\t\t}}\n\n\t\terror = css__stylesheet_style_appendOPV(result, {}, 0, value);\n\t\tif (error != CSS_OK) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn error;\n\t\t}}\n\n\t\tif (value == COLOR_SET)\n\t\t\terror = css__stylesheet_style_append(result, color);\n\t}}\n\n",
            id.val
        ));
    } else if let Some(k) = length_unit.first() {
        o.push_str(&format!(
            "{{\n\t\tcss_fixed length = 0;\n\t\tuint32_t unit = 0;\n\t\t*ctx = orig_ctx;\n\n\t\terror = css__parse_unit_specifier(c, vector, ctx, {}, &length, &unit);\n\t\tif (error != CSS_OK) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn error;\n\t\t}}\n\n",
            k.key
        ));
        for r in length_unit.iter().skip(1) {
            match r.key.as_str() {
                "ALLOW" => o.push_str(&format!(
                    "\t\tif (({}) == false) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn CSS_INVALID;\n\t\t}}\n\n",
                    r.val
                )),
                "DISALLOW" => o.push_str(&format!(
                    "\t\tif ({}) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn CSS_INVALID;\n\t\t}}\n\n",
                    r.val
                )),
                "MASK" => o.push_str(&format!(
                    "\t\tif ((unit & {} ) == 0) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn CSS_INVALID;\n\t\t}}\n\n",
                    r.val
                )),
                "RANGE" => o.push_str(&format!(
                    "\t\tif (length {}) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn CSS_INVALID;\n\t\t}}\n\n",
                    r.val
                )),
                _ => {}
            }
        }
        o.push_str(&format!(
            "\t\terror = css__stylesheet_style_appendOPV(result, {}, 0, {});\n\t\tif (error != CSS_OK) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn error;\n\t\t}}\n\n\t\terror = css__stylesheet_style_vappend(result, 2, length, unit);\n\t}}\n\n",
            id.val, k.val
        ));
    } else if !ident_list.is_empty() {
        let k = &ident_list[0];
        assert!(k.key == "STRING_OPTNUM" && ident_list.len() >= 2, "bad IDENT_LIST in {descriptor}");
        let i = &ident_list[1];
        o.push_str(&format!(
            "{{\n\t\terror = css__stylesheet_style_appendOPV(result, {}, 0, {});\n\t\tif (error != CSS_OK) {{\n\t\t\t*ctx = orig_ctx;\n\t\t\treturn error;\n\t\t}}\n\n\t\twhile ((token != NULL) && (token->type == CSS_TOKEN_IDENT)) {{\n\t\t\tuint32_t snumber;\n\t\t\tcss_fixed num;\n\t\t\tint pctx;\n\n\t\t\terror = css__stylesheet_string_add(c->sheet, lwc_string_ref(token->idata), &snumber);\n\t\t\tif (error != CSS_OK) {{\n\t\t\t\t*ctx = orig_ctx;\n\t\t\t\treturn error;\n\t\t\t}}\n\n\t\t\terror = css__stylesheet_style_append(result, snumber);\n\t\t\tif (error != CSS_OK) {{\n\t\t\t\t*ctx = orig_ctx;\n\t\t\t\treturn error;\n\t\t\t}}\n\n\t\t\tconsumeWhitespace(vector, ctx);\n\n\t\t\tpctx = *ctx;\n\t\t\ttoken = parserutils_vector_iterate(vector, ctx);\n\t\t\tif ((token != NULL) && (token->type == CSS_TOKEN_NUMBER)) {{\n\t\t\t\tsize_t consumed = 0;\n\n\t\t\t\tnum = css__number_from_lwc_string(token->idata, true, &consumed);\n\t\t\t\tif (consumed != lwc_string_length(token->idata)) {{\n\t\t\t\t\t*ctx = orig_ctx;\n\t\t\t\t\treturn CSS_INVALID;\n\t\t\t\t}}\n\t\t\t\tconsumeWhitespace(vector, ctx);\n\n\t\t\t\tpctx = *ctx;\n\t\t\t\ttoken = parserutils_vector_iterate(vector, ctx);\n\t\t\t}} else {{\n\t\t\t\tnum = INTTOFIX({});\n\t\t\t}}\n\n\t\t\terror = css__stylesheet_style_append(result, num);\n\t\t\tif (error != CSS_OK) {{\n\t\t\t\t*ctx = orig_ctx;\n\t\t\t\treturn error;\n\t\t\t}}\n\n\t\t\tif (token == NULL)\n\t\t\t\tbreak;\n\n\t\t\tif (token->type == CSS_TOKEN_IDENT) {{\n\t\t\t\terror = css__stylesheet_style_append(result, {});\n\t\t\t\tif (error != CSS_OK) {{\n\t\t\t\t\t*ctx = orig_ctx;\n\t\t\t\t\treturn error;\n\t\t\t\t}}\n\t\t\t}} else {{\n\t\t\t\t*ctx = pctx; /* rewind one token back */\n\t\t\t}}\n\t\t}}\n\n\t\terror = css__stylesheet_style_append(result, {});\n\t}}\n\n",
            id.val, k.val, i.key, k.val, i.val
        ));
    } else {
        o.push_str("{\n\t\terror = CSS_INVALID;\n\t}\n\n");
    }
    o.push_str("\tif (error != CSS_OK)\n\t\t*ctx = orig_ctx;\n\t\n\treturn error;\n}\n\n");
    o
}

/// A framebuffer picture as C (`tools/convert_image.c`): RGBA bytes, and
/// for pointers (a green hotspot marker in the top row and left column),
/// the hotspot.
fn gen_image(png: &Path, name: &str) -> String {
    let decoder = {
        let mut d = ::png::Decoder::new(fs::File::open(png).unwrap());
        d.set_transformations(::png::Transformations::normalize_to_color8() | ::png::Transformations::ALPHA);
        d
    };
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    let (w, h) = (info.width as usize, info.height as usize);
    // Grey pictures come out as grey + alpha: make them RGBA.
    let rgba: Vec<u8> = match info.color_type {
        ::png::ColorType::Rgba => buf[..w * h * 4].to_vec(),
        ::png::ColorType::GrayscaleAlpha => buf[..w * h * 2].chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        other => panic!("{}: unexpected PNG colour type {other:?}", png.display()),
    };
    let px = |x: usize, y: usize| &rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
    // A pointer's top row and left column are a frame with one green pixel each.
    let hotspot = || -> Option<(usize, usize)> {
        let marker = |p: &[u8]| -> Option<bool> {
            match p[3] {
                255 if p[0] == 0 && p[2] == 0 => Some(p[1] == 255),
                0 => Some(false),
                _ => None,
            }
        };
        let row: Vec<bool> = (0..w).map(|x| marker(px(x, 0))).collect::<Option<_>>()?;
        let col: Vec<bool> = (0..h).map(|y| marker(px(0, y))).collect::<Option<_>>()?;
        let (hx, hy) = (row.iter().filter(|g| **g).count(), col.iter().filter(|g| **g).count());
        (hx == 1 && hy == 1).then(|| (row.iter().position(|g| *g).unwrap(), col.iter().position(|g| *g).unwrap()))
    };
    let hot = hotspot();
    let (x0, y0) = if hot.is_some() { (1, 1) } else { (0, 0) };
    let (ow, oh) = (w - x0, h - y0);
    let mut out = format!(
        "/* This file is auto-generated from {}\n *\n * Do not edit this file directly.\n */\n\n#include <sys/types.h>\n\n#include <stdint.h>\n\n#include <stdbool.h>\n\n#include <libnsfb.h>\n\n#include \"netsurf/plot_style.h\"\n#include \"framebuffer/gui.h\"\n#include \"framebuffer/fbtk.h\"\n\nstatic uint8_t {name}_pixdata[] = {{\n",
        png.display()
    );
    for y in y0..h {
        out.push('\t');
        for x in x0..w {
            for b in px(x, y) {
                out.push_str(&format!("0x{b:02x}, "));
            }
        }
        out.push('\n');
    }
    let (hx, hy) = hot.map_or((0, 0), |(x, y)| (x as i32 - 1, y as i32 - 1));
    out.push_str(&format!(
        "}};\n\nstruct fbtk_bitmap {name} = {{\n\t.width\t\t= {ow},\n\t.height\t\t= {oh},\n\t.hot_x\t\t= {hx},\n\t.hot_y\t\t= {hy},\n\t.pixdata\t= {name}_pixdata,\n}};\n\n"
    ));
    out
}

/// NetSurf's messages in `lang` for `platform` (and "all"), from
/// `resources/FatMessages` (`tools/split-messages.c`).
fn gen_messages(fat: &str, lang: &str, platform: &str) -> String {
    let mut out = String::new();
    for line in fat.lines() {
        let l = line.trim_start();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let Some((key, value)) = l.split_once(':') else { continue };
        let mut parts = key.splitn(3, '.');
        let (Some(lg), Some(plat), Some(tok)) = (parts.next(), parts.next(), parts.next()) else { continue };
        if lg == lang && (plat == "all" || plat == platform) {
            out.push_str(&format!("{tok}:{value}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_table() {
        let src = "%language=ANSI-C\n%ignore-case\n%define lookup-function-name find\n%{\n#include <string.h>\n%}\nstruct thing;\n%%\nB, 2\na, 1\n";
        let out = gen_lookup(src, true);
        assert!(out.contains("{ \"a\", 1 },\n\t{ \"B\", 2 }"));
        assert!(out.contains("static const struct thing *\nfind("));
        assert!(out.contains("strncasecmp"));
    }

    #[test]
    fn entities_tree() {
        let out = gen_entities("# c\namp;\t0x0026\nAMP;\t0x0026\namp\t0x0026\n");
        assert!(out.contains("static int32_t dict_root = "));
        // Every key ends in a node that carries its value.
        assert!(out.matches("0x0026").count() >= 3);
    }

    #[test]
    fn css_parser() {
        let out = gen_css_parser("list_style_position:CSS_PROP_LIST_STYLE_POSITION IDENT:( INHERIT: INSIDE:0,LIST_STYLE_POSITION_INSIDE OUTSIDE:0,LIST_STYLE_POSITION_OUTSIDE IDENT:)");
        assert!(out.contains("css_error css__parse_list_style_position(css_language *c,"));
        assert!(out.contains("css_stylesheet_style_inherit(result,\n\t\t\t\tCSS_PROP_LIST_STYLE_POSITION)"));
        assert!(out.contains("c->strings[OUTSIDE]"));
        assert!(out.contains("error = CSS_INVALID;"));
    }

    #[test]
    fn messages() {
        let out = gen_messages("# x\nen.all.A:one\nde.all.A:eins\nen.gtk.B:two\nen.fb.C:three\n", "en", "fb");
        assert_eq!(out, "A:one\nC:three\n");
    }
}
