"""Writes assets/notices/THIRD-PARTY-NOTICES.txt for the installed app.

Collects the MapLibre license, the VersaTiles style notice, the font and icon licenses,
the Rust crates linked into offline_maps_ffi.dll (from `cargo tree`) and the .NET
packages. Run after changing dependencies: python tools/third-party-notices.py
"""
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "assets" / "notices" / "THIRD-PARTY-NOTICES.txt"

OFL = """SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007

PREAMBLE
The goals of the Open Font License (OFL) are to stimulate worldwide development of
collaborative font projects, to support the font creation efforts of academic and
linguistic communities, and to provide a free and open framework in which fonts may be
shared and improved in partnership with others.

The OFL allows the licensed fonts to be used, studied, modified and redistributed freely
as long as they are not sold by themselves. The fonts, including any derivative works,
can be bundled, embedded, redistributed and/or sold with any software provided that any
reserved names are not used by derivative works. The fonts and derivatives, however,
cannot be released under any other type of license. The requirement for fonts to remain
under this license does not apply to any document created using the fonts or their
derivatives.

DEFINITIONS
"Font Software" refers to the set of files released by the Copyright Holder(s) under this
license and clearly marked as such. This may include source files, build scripts and
documentation.

"Reserved Font Name" refers to any names specified as such after the copyright
statement(s).

"Original Version" refers to the collection of Font Software components as distributed by
the Copyright Holder(s).

"Modified Version" refers to any derivative made by adding to, deleting, or substituting
-- in part or in whole -- any of the components of the Original Version, by changing
formats or by porting the Font Software to a new environment.

"Author" refers to any designer, engineer, programmer, technical writer or other person
who contributed to the Font Software.

PERMISSION & CONDITIONS
Permission is hereby granted, free of charge, to any person obtaining a copy of the Font
Software, to use, study, copy, merge, embed, modify, redistribute, and sell modified and
unmodified copies of the Font Software, subject to the following conditions:

1) Neither the Font Software nor any of its individual components, in Original or
Modified Versions, may be sold by itself.

2) Original or Modified Versions of the Font Software may be bundled, redistributed
and/or sold with any software, provided that each copy contains the above copyright
notice and this license. These can be included either as stand-alone text files,
human-readable headers or in the appropriate machine-readable metadata fields within
text or binary files as long as those fields can be easily viewed by the user.

3) No Modified Version of the Font Software may use the Reserved Font Name(s) unless
explicit written permission is granted by the corresponding Copyright Holder. This
restriction only applies to the primary font name as presented to the users.

4) The name(s) of the Copyright Holder(s) and the Author(s) of the Font Software shall
not be used to promote, endorse or advertise any Modified Version, except to acknowledge
the contribution(s) of the Copyright Holder(s) and the Author(s) or with their explicit
written permission.

5) The Font Software, modified or unmodified, in part or in whole, must be distributed
entirely under this license, and must not be distributed under any other license. The
requirement for fonts to remain under this license does not apply to any document
created using the Font Software.

TERMINATION
This license becomes null and void if any of the above conditions are not met.

DISCLAIMER
THE FONT SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
INCLUDING BUT NOT LIMITED TO ANY WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
PURPOSE AND NONINFRINGEMENT OF COPYRIGHT, PATENT, TRADEMARK, OR OTHER RIGHT. IN NO EVENT
SHALL THE COPYRIGHT HOLDER BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, INCLUDING
ANY GENERAL, SPECIAL, INDIRECT, INCIDENTAL, OR CONSEQUENTIAL DAMAGES, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF THE USE OR INABILITY TO USE THE FONT
SOFTWARE OR FROM OTHER DEALINGS IN THE FONT SOFTWARE.
"""

NUGET = [
    ("Microsoft.WindowsAppSDK 2.5.1 (Windows App SDK runtime, bundled)", "MIT; runtime components under the Microsoft Software License Terms"),
    ("Microsoft.Web.WebView2 (through the Windows App SDK)", "BSD-3-Clause (SDK); the WebView2 Runtime is installed separately under Microsoft terms"),
    ("CommunityToolkit.WinUI.Controls.Sizers 8.2.251219", "MIT"),
    ("Google.Protobuf 3.36.2", "BSD-3-Clause"),
    (".NET 10 runtime (bundled)", "MIT"),
    ("Jupiter.UI 1.1.0 (Ian Rastall)", "Jupiter shared library"),
]


def rust_crates() -> list[str]:
    out = subprocess.run(
        ["cargo", "tree", "-p", "offline-maps-ffi", "-e", "normal", "--prefix", "none", "-f", "{p}\t{l}"],
        cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout
    crates = set()
    for line in out.splitlines():
        line = line.replace(" (*)", "").strip()
        if not line or "offline-maps" in line or "(proc-macro)" in line:
            continue
        name_version, _, license_ = line.partition("\t")
        name_version = re.sub(r" \(.*\)$", "", name_version)
        crates.add(f"{name_version}: {license_ or 'see crate'}")
    return sorted(crates)


def main() -> None:
    maplibre = (ROOT / "proof/renderer/node_modules/maplibre-gl/LICENSE.txt").read_text(encoding="utf-8")
    contour = (ROOT / "proof/renderer/node_modules/maplibre-contour/LICENSE").read_text(encoding="utf-8")
    versatiles = (ROOT / "assets/styles/NOTICE.md").read_text(encoding="utf-8")
    versatiles = versatiles.replace("```text\n", "").replace("```", "")
    sections = [
        "Offline North American Maps third-party notices\n\n"
        "Offline North American Maps' own code is released into the public domain under The Unlicense "
        "(https://unlicense.org). It contains the components below, which keep their own licenses. "
        "Map data is not part of the program: it is "
        "downloaded by the person using it from Geofabrik and is (c) OpenStreetMap contributors, "
        "available under the Open Database License (ODbL) 1.0, https://www.openstreetmap.org/copyright. "
        "Terrain, when downloaded, comes from Mapterhorn (https://mapterhorn.com/attribution): the USGS "
        "3D Elevation Program in the United States (public domain) and Copernicus GLO-30 "
        "((c) DLR e.V. 2010-2014 and (c) Airbus Defence and Space GmbH 2014-2018, provided under "
        "COPERNICUS by the European Union and ESA; all rights reserved). Flood zones and public lands, "
        "when downloaded, are prepared from FEMA's National Flood Hazard Layer and the USGS Protected "
        "Areas Database of the United States (PAD-US), US Government works in the public domain. "
        "The built-in overview of the United States and Mexico is made from Natural Earth (naturalearthdata.com), "
        "public domain.",
        "MapLibre GL JS 6.16.0 (map renderer)\n\n" + maplibre.strip(),
        "maplibre-contour 0.1.1 (contour lines from terrain)\n\n" + contour.strip(),
        versatiles.strip().lstrip("# "),
        "VersaTiles sprites (map icons), from versatiles-style 7.0.1 sprites.tar.gz: CC0 1.0 Universal "
        "(public domain dedication), https://creativecommons.org/publicdomain/zero/1.0/",
        "Noto Sans (map label glyphs, as packaged by VersaTiles Fonts 3.0.0, MIT)\n"
        "Copyright 2022 The Noto Project Authors (https://github.com/notofonts/latin-greek-cyrillic)\n\n" + OFL.strip(),
        "Rust crates in offline_maps_ffi.dll (licenses as declared by each crate; the MIT and "
        "Apache-2.0 texts are at https://opensource.org/license/mit and "
        "https://www.apache.org/licenses/LICENSE-2.0). SQLite is in the public domain.\n\n"
        + "\n".join(rust_crates()),
        ".NET packages\n\n" + "\n".join(f"{name}: {license_}" for name, license_ in NUGET),
    ]
    OUT.parent.mkdir(parents=True, exist_ok=True)
    rule = "\n\n" + "=" * 78 + "\n\n"
    OUT.write_text(rule.join(sections) + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
