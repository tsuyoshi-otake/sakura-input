[CmdletBinding()]
param(
    [switch]$SelfTest,
    [switch]$Report,
    [string]$Root
)

# R10 (docs/architecture/agent-refactor-plan.md): every flat re-export in the
# sakura-core facade must have an evidenced caller. Callers are resolved on
# code with comments and string/char literals blanked, by the first path
# segment after `sakura_core::` (or `crate::` inside sakura-core), so a
# comment, a string, or a same-named item under another path never counts.
# Doc-comment code fences count, because doc tests compile. Anything the
# resolver cannot see through (a glob import) counts as a caller for every
# item: the check may keep a dead export, never report a live one as dead.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $Root) { $Root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..')) }

if (-not ('SakuraFacade.RustLexer' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Text;

namespace SakuraFacade {
public static class RustLexer {
    static bool IsIdent(char c) { return char.IsLetterOrDigit(c) || c == '_'; }

    static void Blank(StringBuilder o, string s, int from, int to) {
        for (int k = from; k < to; k++) o.Append(s[k] == '\n' ? '\n' : ' ');
    }

    static bool IsCodeFence(string lang) {
        if (lang.Length == 0) return true;
        foreach (var tag in lang.Split(',')) {
            var t = tag.Trim();
            if (!(t == "rust" || t == "no_run" || t == "ignore" || t == "should_panic"
                || t == "compile_fail" || t.StartsWith("edition"))) return false;
        }
        return true;
    }

    // Returns `s` with comments and string/char literals blanked; newlines and
    // offsets are preserved. Code inside rust fences of `///` and `//!`
    // comments is kept.
    public static string CodeOnly(string s) {
        var o = new StringBuilder(s.Length);
        int i = 0, n = s.Length;
        bool inFence = false, codeFence = false;
        while (i < n) {
            char c = s[i];
            if (c == '/' && i + 1 < n && s[i + 1] == '/') {
                int end = s.IndexOf('\n', i);
                if (end < 0) end = n;
                bool doc = i + 2 < n && (s[i + 2] == '!' || (s[i + 2] == '/' && !(i + 3 < n && s[i + 3] == '/')));
                if (doc) {
                    string body = s.Substring(i + 3, end - (i + 3));
                    string trimmed = body.Trim();
                    if (trimmed.StartsWith("```")) {
                        if (inFence) { inFence = false; codeFence = false; }
                        else { inFence = true; codeFence = IsCodeFence(trimmed.Substring(3).Trim()); }
                        Blank(o, s, i, end);
                    } else if (inFence && codeFence) {
                        o.Append("   ");
                        o.Append(CodeOnly(body));
                    } else {
                        Blank(o, s, i, end);
                    }
                } else {
                    Blank(o, s, i, end);
                }
                i = end;
                continue;
            }
            if (c == '/' && i + 1 < n && s[i + 1] == '*') {
                int depth = 0, j = i;
                while (j < n) {
                    if (s[j] == '/' && j + 1 < n && s[j + 1] == '*') { depth++; j += 2; continue; }
                    if (s[j] == '*' && j + 1 < n && s[j + 1] == '/') { depth--; j += 2; if (depth == 0) break; continue; }
                    j++;
                }
                Blank(o, s, i, j);
                i = j;
                continue;
            }
            char prev = i > 0 ? s[i - 1] : ' ';
            if (!IsIdent(prev) && (c == 'r' || ((c == 'b' || c == 'c') && i + 1 < n && s[i + 1] == 'r'))) {
                int j = i + (c == 'r' ? 1 : 2), hashes = 0;
                while (j < n && s[j] == '#') { hashes++; j++; }
                if (j < n && s[j] == '"') {
                    string close = "\"" + new string('#', hashes);
                    int end = s.IndexOf(close, j + 1, StringComparison.Ordinal);
                    end = end < 0 ? n : end + close.Length;
                    Blank(o, s, i, end);
                    i = end;
                    continue;
                }
            }
            if (c == '"') {
                int j = i + 1;
                while (j < n && s[j] != '"') { if (s[j] == '\\') j++; j++; }
                int end = Math.Min(n, j + 1);
                Blank(o, s, i, end);
                i = end;
                continue;
            }
            if (c == '\'') {
                if (i + 1 < n && s[i + 1] == '\\') {
                    int j = i + 3;
                    while (j < n && s[j] != '\'' && s[j] != '\n') j++;
                    int end = Math.Min(n, j + 1);
                    Blank(o, s, i, end);
                    i = end;
                    continue;
                }
                int len = i + 1 < n && char.IsHighSurrogate(s[i + 1]) ? 2 : 1;
                if (i + 1 + len < n && s[i + 1 + len] == '\'') {
                    Blank(o, s, i, i + 2 + len);
                    i += 2 + len;
                    continue;
                }
                // A lifetime or loop label.
            }
            o.Append(c);
            i++;
        }
        return o.ToString();
    }
}
}
'@
}

function Get-FilesUnder([string]$Directory, [string]$Pattern) {
    $found = [Collections.Generic.List[string]]::new()
    if (-not [IO.Directory]::Exists($Directory)) { return , $found }
    $skip = @('target', '.git', 'node_modules', '.claude')
    $stack = [Collections.Generic.Stack[string]]::new()
    $stack.Push([IO.Path]::GetFullPath($Directory))
    while ($stack.Count -gt 0) {
        $dir = $stack.Pop()
        foreach ($sub in [IO.Directory]::EnumerateDirectories($dir)) {
            if ($skip -contains [IO.Path]::GetFileName($sub)) { continue }
            if ([IO.File]::GetAttributes($sub) -band [IO.FileAttributes]::ReparsePoint) { continue }
            $stack.Push($sub)
        }
        foreach ($file in [IO.Directory]::EnumerateFiles($dir, $Pattern)) { $found.Add($file) }
    }
    return , $found
}

function Get-RelativePath([string]$RepoRoot, [string]$Path) {
    return ([IO.Path]::GetRelativePath($RepoRoot, $Path) -replace '\\', '/')
}

function Get-LineNumber([string]$Text, [int]$Offset) {
    return $Text.Substring(0, $Offset).Split([char]10).Length
}

function Split-TopLevel([string]$Text) {
    $parts = [Collections.Generic.List[string]]::new()
    $depth = 0
    $start = 0
    for ($k = 0; $k -lt $Text.Length; $k++) {
        $ch = $Text[$k]
        if ($ch -eq [char]'{') { $depth++ }
        elseif ($ch -eq [char]'}') { $depth-- }
        elseif ($ch -eq [char]',' -and $depth -eq 0) {
            $parts.Add($Text.Substring($start, $k - $start).Trim())
            $start = $k + 1
        }
    }
    $parts.Add($Text.Substring($start).Trim())
    return , @($parts | Where-Object { $_ })
}

# First path segments that follow `<root>::`, including every top-level entry
# of a `<root>::{...}` use tree. `Glob` is set by `<root>::*`.
function Get-FirstSegments([string]$Code, [string]$RootPattern) {
    $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $glob = $false
    $pattern = '(?<![\w:$])(?:::\s*)?' + $RootPattern + '\s*::\s*(\{|\*|[A-Za-z_]\w*)'
    foreach ($match in [regex]::Matches($Code, $pattern)) {
        $head = $match.Groups[1]
        if ($head.Value -eq '*') { $glob = $true; continue }
        if ($head.Value -ne '{') { [void]$names.Add($head.Value); continue }
        $depth = 0
        $close = -1
        for ($k = $head.Index; $k -lt $Code.Length; $k++) {
            if ($Code[$k] -eq [char]'{') { $depth++ }
            elseif ($Code[$k] -eq [char]'}') { $depth--; if ($depth -eq 0) { $close = $k; break } }
        }
        if ($close -lt 0) { throw "Unbalanced use tree after '$($match.Value)'" }
        foreach ($part in (Split-TopLevel $Code.Substring($head.Index + 1, $close - $head.Index - 1))) {
            if ($part -eq '*') { $glob = $true; continue }
            if ($part -match '^([A-Za-z_]\w*)' -and $Matches[1] -ne 'self') { [void]$names.Add($Matches[1]) }
        }
    }
    return [pscustomobject]@{ Names = $names; Glob = $glob }
}

function Get-FacadeExports([string]$FacadePath) {
    $code = [SakuraFacade.RustLexer]::CodeOnly([IO.File]::ReadAllText($FacadePath))
    $exports = [Collections.Generic.List[object]]::new()
    foreach ($match in [regex]::Matches($code, '(?<!\w)pub\s+use\s+([^;]+);')) {
        $body = $match.Groups[1].Value
        $base = $match.Groups[1].Index
        if ($body.Contains('*')) { throw "Glob re-exports cannot be audited: pub use $body;" }
        $brace = $body.IndexOf('{')
        if ($brace -lt 0) {
            $single = [regex]::Match($body, '([A-Za-z_]\w*)\s*(?:as\s+([A-Za-z_]\w*))?\s*$')
            $group = if ($single.Groups[2].Success) { $single.Groups[2] } else { $single.Groups[1] }
            $exports.Add([pscustomobject]@{ Name = $group.Value; Line = Get-LineNumber $code ($base + $group.Index) })
            continue
        }
        if ($body.IndexOf('{', $brace + 1) -ge 0) { throw "Nested use trees are not supported in the facade: pub use $body;" }
        $innerStart = $brace + 1
        $inner = $body.Substring($innerStart, $body.LastIndexOf('}') - $innerStart)
        foreach ($item in [regex]::Matches($inner, '([A-Za-z_]\w*)(?:\s+as\s+([A-Za-z_]\w*))?')) {
            $group = if ($item.Groups[2].Success) { $item.Groups[2] } else { $item.Groups[1] }
            if ($group.Value -eq 'self') { continue }
            $exports.Add([pscustomobject]@{ Name = $group.Value; Line = Get-LineNumber $code ($base + $innerStart + $group.Index) })
        }
    }
    return , $exports
}

function Find-FacadeCallers {
    param(
        [string]$RepoRoot,
        [string]$FacadePath,
        [string]$CrateName,
        [string[]]$SourceRoots,
        [string[]]$DocFiles
    )
    $facadePath = [IO.Path]::GetFullPath($FacadePath)
    $crateSrc = [IO.Path]::GetDirectoryName($facadePath) + [IO.Path]::DirectorySeparatorChar
    $exports = Get-FacadeExports $facadePath
    $callers = @{}
    foreach ($export in $exports) { $callers[$export.Name] = [Collections.Generic.SortedSet[string]]::new([StringComparer]::Ordinal) }
    $globs = [Collections.Generic.SortedSet[string]]::new([StringComparer]::Ordinal)

    $record = {
        param($segments, [string]$where)
        foreach ($name in $segments.Names) { if ($callers.ContainsKey($name)) { [void]$callers[$name].Add($where) } }
        if ($segments.Glob) { [void]$globs.Add($where) }
    }

    $files = [Collections.Generic.SortedSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($sourceRoot in $SourceRoots) {
        foreach ($file in (Get-FilesUnder $sourceRoot '*.rs')) { [void]$files.Add([IO.Path]::GetFullPath($file)) }
    }
    foreach ($file in $files) {
        $text = [IO.File]::ReadAllText($file)
        $inCrate = $file.StartsWith($crateSrc, [StringComparison]::OrdinalIgnoreCase)
        if (-not $inCrate -and $text.IndexOf($CrateName, [StringComparison]::Ordinal) -lt 0) { continue }
        $code = [SakuraFacade.RustLexer]::CodeOnly($text)
        $where = Get-RelativePath $RepoRoot $file
        & $record (Get-FirstSegments $code ([regex]::Escape($CrateName))) $where
        if (-not $inCrate) { continue }
        & $record (Get-FirstSegments $code 'crate') $where
        & $record (Get-FirstSegments $code '\$crate') $where
        $relative = $file.Substring($crateSrc.Length) -replace '\\', '/'
        if ($relative -notmatch '/' -or $relative -match '^[^/]+/mod\.rs$') {
            # A top-level module's `super::` is the crate root; inline child
            # modules make this over-count, which only keeps an export.
            $super = Get-FirstSegments $code 'super'
            $super.Glob = $false
            & $record $super $where
        }
        if ($file -ieq $facadePath) {
            $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
            foreach ($use in [regex]::Matches($code, '(?<!\w)use\s+([A-Za-z_]\w*)\s*::')) { [void]$names.Add($use.Groups[1].Value) }
            & $record ([pscustomobject]@{ Names = $names; Glob = $false }) $where
        }
    }
    foreach ($doc in $DocFiles) {
        $text = [IO.File]::ReadAllText($doc)
        if ($text.IndexOf($CrateName, [StringComparison]::Ordinal) -lt 0) { continue }
        & $record (Get-FirstSegments $text ([regex]::Escape($CrateName))) (Get-RelativePath $RepoRoot $doc)
    }

    return @($exports | ForEach-Object {
        [pscustomobject]@{
            Name = $_.Name
            Line = $_.Line
            Callers = @($callers[$_.Name])
            Used = $callers[$_.Name].Count -gt 0 -or $globs.Count -gt 0
        }
    })
}

function Get-DocFiles([string]$RepoRoot) {
    # Get-FilesUnder returns its list as one pipeline object; loop instead.
    $docs = [Collections.Generic.List[string]]::new()
    foreach ($file in (Get-FilesUnder $RepoRoot '*.md')) {
        if ((Get-RelativePath $RepoRoot $file) -notmatch '^docs/history/') { $docs.Add($file) }
    }
    return , $docs.ToArray()
}

function Get-WorkspaceSourceRoots([string]$RepoRoot) {
    $cargo = Get-Command -Name 'cargo' -CommandType Application -ErrorAction Stop | Select-Object -First 1
    $json = & $cargo.Source metadata --format-version 1 --no-deps --manifest-path (Join-Path $RepoRoot 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed with exit code $LASTEXITCODE" }
    $metadata = ($json -join "`n") | ConvertFrom-Json
    $roots = [Collections.Generic.SortedSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($package in $metadata.packages) {
        [void]$roots.Add([IO.Path]::GetDirectoryName($package.manifest_path))
        foreach ($target in $package.targets) { [void]$roots.Add([IO.Path]::GetDirectoryName($target.src_path)) }
    }
    # Tools outside the workspace (candidate-sweep, candidate-snapshot) also
    # build against sakura-core.
    $tools = Join-Path $RepoRoot 'tools'
    if ([IO.Directory]::Exists($tools)) { [void]$roots.Add($tools) }
    return @($roots)
}

function Invoke-SelfTest {
    $fixture = Join-Path ([IO.Path]::GetTempPath()) ('sakura-facade-selftest-' + [Guid]::NewGuid().ToString('N'))
    $write = {
        param([string]$Relative, [string]$Text)
        $path = Join-Path $fixture $Relative
        [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path))
        [IO.File]::WriteAllText($path, $Text)
    }
    try {
        & $write 'crates/core/src/lib.rs' @'
//! Facade. `sakura_core::Commented` in a crate doc is not a caller.
pub mod m;
pub mod internal_user;
pub use m::{
    AfterChar, AfterLifetime, Aliased as Renamed, Commented, DocUsed, HistoryOnly, Internal, MdUsed,
    Nested, RawAfter, SameName, Stringed, Unused, Used,
};
pub use m::sub as alias_mod;
pub use alias_mod::ViaAlias;
'@
        & $write 'crates/core/src/internal_user.rs' @'
use crate::{Internal};
/// ```
/// let x = sakura_core::DocUsed;
/// ```
/// ```text
/// sakura_core::Unused
/// ```
pub fn f() {}
'@
        & $write 'crates/app/src/main.rs' @'
use sakura_core::{Used, m::SameName as Other, Nested as N};
// sakura_core::Commented
/* sakura_core::Commented /* nested */ sakura_core::Commented */
fn g<'a>(x: &'a str) -> &'a str { let _ = sakura_core::AfterLifetime; x }
fn h() {
    let q = '"';
    let _ = sakura_core::AfterChar;
    let s = "sakura_core::Stringed \" sakura_core::Stringed";
    let r = r##"sakura_core::Stringed "# still"##;
    let _ = sakura_core::RawAfter;
    let _ = other::SameName;
    let _ = sakura_core::Renamed;
}
'@
        & $write 'docs/guide.md' 'Call `sakura_core::MdUsed`.'
        & $write 'docs/history/old.md' 'Dated: `sakura_core::HistoryOnly`.'

        $run = {
            Find-FacadeCallers -RepoRoot $fixture -FacadePath (Join-Path $fixture 'crates/core/src/lib.rs') `
                -CrateName 'sakura_core' -SourceRoots @((Join-Path $fixture 'crates')) -DocFiles (Get-DocFiles $fixture)
        }
        $unused = (@((& $run) | Where-Object { -not $_.Used } | ForEach-Object Name) | Sort-Object) -join ','
        $expected = 'Commented,HistoryOnly,SameName,Stringed,Unused,ViaAlias'
        if ($unused -cne $expected) { throw "R10 self-test: expected unused [$expected], got [$unused]" }

        & $write 'crates/app/src/glob.rs' 'use sakura_core::*;'
        $afterGlob = @((& $run) | Where-Object { -not $_.Used }).Count
        if ($afterGlob -ne 0) { throw "R10 self-test: a glob import must count as a caller of every export, $afterGlob left unused" }
    }
    finally {
        if ([IO.Directory]::Exists($fixture)) { [IO.Directory]::Delete($fixture, $true) }
    }
    Write-Host 'PASS: R10 facade fixtures cover comments, nested block comments, strings, raw strings, char literals vs lifetimes, doc-test fences, same-named paths, aliases, crate:: callers, docs outside history, and glob imports'
}

if ($SelfTest) {
    Invoke-SelfTest
    return
}

$facade = Join-Path $Root 'crates/sakura-core/src/lib.rs'
$results = Find-FacadeCallers -RepoRoot $Root -FacadePath $facade -CrateName 'sakura_core' `
    -SourceRoots (Get-WorkspaceSourceRoots $Root) -DocFiles (Get-DocFiles $Root)
if ($Report) {
    foreach ($result in $results) {
        $sample = @($result.Callers | Select-Object -First 3) -join ', '
        Write-Host ('{0,-42} lib.rs:{1,-3} callers={2,-3} {3}' -f $result.Name, $result.Line, $result.Callers.Count, $sample)
    }
}
$unused = @($results | Where-Object { -not $_.Used })
if ($unused.Count -gt 0) {
    foreach ($item in $unused) {
        Write-Host "FAIL: R10 facade re-export has no caller: $($item.Name) (crates/sakura-core/src/lib.rs:$($item.Line))"
    }
    exit 1
}
Write-Host "PASS: R10 every sakura_core facade re-export has an evidenced caller ($($results.Count) items)"
