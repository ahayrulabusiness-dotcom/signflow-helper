#!/bin/bash
# The stand-in for "the app this Mac uses for .fs files": an AppleScript
# applet that claims .fs (through its own exported type), registered with
# Launch Services, and that writes each file it is asked to open into
# <opened.txt>. Runner only.
#   e2e/mac/make-fake-editor.sh <opened.txt> <a probe .fs path>
set -euo pipefail
OPENED="$1"
APP="/Applications/SignFlowE2E Editor.app"
LSREG=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
SRC=$(mktemp -d)
rm -rf "$APP"
cat > "$SRC/editor.applescript" <<EOS
on open theFiles
  repeat with f in theFiles
    do shell script "echo " & quoted form of (POSIX path of f) & " >> " & quoted form of "$OPENED"
  end repeat
end open
EOS
osacompile -o "$APP" "$SRC/editor.applescript"
PL="$APP/Contents/Info.plist"
PB=/usr/libexec/PlistBuddy
$PB -c "Set :CFBundleIdentifier com.signflow.e2e.editor" "$PL" 2>/dev/null || $PB -c "Add :CFBundleIdentifier string com.signflow.e2e.editor" "$PL"
$PB -c "Delete :CFBundleDocumentTypes" "$PL" 2>/dev/null || true
$PB -c "Add :CFBundleDocumentTypes array" \
    -c "Add :CFBundleDocumentTypes:0 dict" \
    -c "Add :CFBundleDocumentTypes:0:CFBundleTypeName string SignFlowE2EFile" \
    -c "Add :CFBundleDocumentTypes:0:CFBundleTypeRole string Editor" \
    -c "Add :CFBundleDocumentTypes:0:LSHandlerRank string Owner" \
    -c "Add :CFBundleDocumentTypes:0:LSItemContentTypes array" \
    -c "Add :CFBundleDocumentTypes:0:LSItemContentTypes:0 string com.signflow.e2e.fs" \
    -c "Add :CFBundleDocumentTypes:0:CFBundleTypeExtensions array" \
    -c "Add :CFBundleDocumentTypes:0:CFBundleTypeExtensions:0 string fs" "$PL"
$PB -c "Delete :UTExportedTypeDeclarations" "$PL" 2>/dev/null || true
$PB -c "Add :UTExportedTypeDeclarations array" \
    -c "Add :UTExportedTypeDeclarations:0 dict" \
    -c "Add :UTExportedTypeDeclarations:0:UTTypeIdentifier string com.signflow.e2e.fs" \
    -c "Add :UTExportedTypeDeclarations:0:UTTypeDescription string SignFlow test file" \
    -c "Add :UTExportedTypeDeclarations:0:UTTypeConformsTo array" \
    -c "Add :UTExportedTypeDeclarations:0:UTTypeConformsTo:0 string public.data" \
    -c "Add :UTExportedTypeDeclarations:0:UTTypeTagSpecification dict" \
    -c "Add :UTExportedTypeDeclarations:0:UTTypeTagSpecification:public.filename-extension array" \
    -c "Add :UTExportedTypeDeclarations:0:UTTypeTagSpecification:public.filename-extension:0 string fs" "$PL"
codesign --force --deep --sign - "$APP"
"$LSREG" -f "$APP"
# Make it the default for whatever type this Mac gives .fs files (the runner
# may already have an app for .fs, e.g. for F# source), and for its own type.
# Best effort: the run checks the result itself.
cat > "$SRC/default.swift" <<'EOS'
import AppKit
import CoreServices
import UniformTypeIdentifiers
var types = ["com.signflow.e2e.fs"]
if let t = UTType(filenameExtension: "fs") { types.append(t.identifier) }
for t in Set(types) {
  let r = LSSetDefaultRoleHandlerForContentType(t as CFString, .all, "com.signflow.e2e.editor" as CFString)
  print("default handler for \(t): \(r == 0 ? "set" : "error \(r)")")
}
let probe = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "/tmp/probe.fs")
print("a .fs file is type:", UTType(filenameExtension: "fs")?.identifier ?? "none")
print("opens with:", NSWorkspace.shared.urlForApplication(toOpen: probe)?.path ?? "nothing")
EOS
touch "$2"
# macOS 26.4+ may ask before a default app changes; never wait on that.
perl -e 'alarm 90; exec @ARGV' swift "$SRC/default.swift" "$2" || echo "could not set the default handler (or it timed out)"
echo "fake editor ready: $APP"
