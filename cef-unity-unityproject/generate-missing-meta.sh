#!/usr/bin/env bash
#
# .meta を持たない Unity アセットに、決定論的な GUID の meta を生成する。
#
# 背景: CI (.github/workflows/rust-build.yml の publish job) は Unity Editor を
# 起動せずバイナリだけ差し替えるため、CEF 更新などでファイルが増えると .meta の
# 無いアセットがコミットされる。その状態でパッケージを取り込むと、利用側の Unity
# が GUID をローカル生成し、環境ごとに参照がぶれてしまう。
#
# GUID は Unity プロジェクトルートからの相対パスの md5 で、32 桁 16 進という Unity
# の形式にそのまま合致する。実行するマシンや回数によらず常に同じ値になる。
#
# 使い方:
#   generate-missing-meta.sh <ディレクトリ>           不足している meta を生成する
#   generate-missing-meta.sh --check <ディレクトリ>   生成せず検査する (不足があれば exit 1)

set -euo pipefail

check_only=false
if [ "${1:-}" = "--check" ]; then
    check_only=true
    shift
fi

if [ $# -ne 1 ]; then
    echo "使い方: $0 [--check] <ディレクトリ>" >&2
    exit 2
fi

target_directory="$1"
if [ ! -d "$target_directory" ]; then
    echo "ディレクトリが見つかりません: $target_directory" >&2
    exit 2
fi

# 絶対パスに正規化する。find の出力を後でルート相対へ削るため、呼び出し時に相対パス
# を渡されても GUID が変わらないようにするのが目的。
target_directory="$(cd "$target_directory" && pwd)"

# GUID は Unity プロジェクトルート相対のパスから作るので、チェックアウト先の絶対
# パスが違っても同じ値になる。ルートは Assets と ProjectSettings を持つ祖先。
unity_project_root="$target_directory"
while [ ! -d "$unity_project_root/Assets" ] || [ ! -d "$unity_project_root/ProjectSettings" ]; do
    parent_directory="$(dirname "$unity_project_root")"
    if [ "$parent_directory" = "$unity_project_root" ]; then
        echo "Unity プロジェクトルートが見つかりません: $target_directory" >&2
        exit 2
    fi
    unity_project_root="$parent_directory"
done

compute_guid() {
    local relative_path="$1"
    if command -v md5sum >/dev/null 2>&1; then
        printf '%s' "$relative_path" | md5sum | cut -d ' ' -f 1
    else
        printf '%s' "$relative_path" | md5 -q
    fi
}

# win-x64 と win-arm64 には同名の dll (libcef.dll 等) が並ぶ。インポーター設定が
# 無いと Unity が両方を同じプラットフォーム向けと見なして "同名プラグインが複数ある"
# と衝突扱いにするため、ネイティブ dll には CPU を明示した PluginImporter を書く。
#
# Editor は x64 のみ有効にする (x64 Editor が ARM64 の dll を掴まないようにするため。
# ARM64 の Unity Editor で開発する場合はここを見直す必要がある)。
plugin_cpu_for() {
    local relative_path="$1"
    case "$relative_path" in
        */Plugins/win-x64/*.dll) echo "x86_64" ;;
        */Plugins/win-arm64/*.dll) echo "ARM64" ;;
        *) echo "" ;;
    esac
}

write_plugin_importer() {
    local cpu="$1"
    local editor_enabled=0
    local editor_cpu="None"
    if [ "$cpu" = "x86_64" ]; then
        editor_enabled=1
        editor_cpu="x86_64"
    fi
    cat <<EOF
PluginImporter:
  externalObjects: {}
  serializedVersion: 3
  iconMap: {}
  executionOrder: {}
  defineConstraints: []
  isPreloaded: 0
  isOverridable: 0
  isExplicitlyReferenced: 0
  validateReferences: 1
  platformData:
    Any:
      enabled: 0
      settings:
        Exclude Editor: $((1 - editor_enabled))
        Exclude Linux64: 1
        Exclude OSXUniversal: 1
        Exclude Win: 1
        Exclude Win64: 0
    Editor:
      enabled: $editor_enabled
      settings:
        CPU: $editor_cpu
        DefaultValueInitialized: true
        OS: Windows
    Linux64:
      enabled: 0
      settings:
        CPU: None
    OSXUniversal:
      enabled: 0
      settings:
        CPU: None
    Win:
      enabled: 0
      settings:
        CPU: None
    Win64:
      enabled: 1
      settings:
        CPU: $cpu
  userData:
  assetBundleName:
  assetBundleVariant:
EOF
}

# ネイティブ dll 以外は最小形式 (fileFormatVersion + guid) に留め、インポーター種別の
# 判定は Unity に任せる。
write_meta() {
    local asset_path="$1"
    local existing_guid="${2:-}"
    local relative_path="${asset_path#"$unity_project_root/"}"
    local guid="${existing_guid:-$(compute_guid "$relative_path")}"
    local cpu
    cpu="$(plugin_cpu_for "$relative_path")"

    {
        echo "fileFormatVersion: 2"
        echo "guid: $guid"
        if [ -d "$asset_path" ]; then
            echo "folderAsset: yes"
            echo "DefaultImporter:"
            echo "  externalObjects: {}"
            echo "  userData: "
            echo "  assetBundleName: "
            echo "  assetBundleVariant: "
        elif [ -n "$cpu" ]; then
            write_plugin_importer "$cpu"
        fi
    } >"$asset_path.meta"
}

# 既存 meta が CPU 設定を持たないネイティブ dll か (GUID は保持して書き直す)。
needs_plugin_importer() {
    local asset_path="$1"
    local relative_path="${asset_path#"$unity_project_root/"}"
    [ -n "$(plugin_cpu_for "$relative_path")" ] || return 1
    ! grep -q "PluginImporter:" "$asset_path.meta"
}

read_guid() {
    sed -n 's/^guid: \([0-9a-f]*\).*/\1/p' "$1" | head -1
}

missing_count=0
upgrade_count=0

# 除外するもの:
#  - ドットで始まる名前: Unity がインポート対象外とするため meta を持たない
#  - *.framework / *.bundle の中身: Unity は束ね全体を 1 アセットとして扱う。
#    束ね自体には meta が要るので -prune -print0 で自身は出力する
while IFS= read -r -d '' asset_path; do
    relative_path="${asset_path#"$unity_project_root/"}"

    if [ -e "$asset_path.meta" ]; then
        # 既存 meta でも、CPU 設定を持たないネイティブ dll は衝突の原因になるので直す
        if needs_plugin_importer "$asset_path"; then
            upgrade_count=$((upgrade_count + 1))
            if [ "$check_only" = true ]; then
                echo "CPU 設定なし: $relative_path"
            else
                write_meta "$asset_path" "$(read_guid "$asset_path.meta")"
                echo "CPU 設定を追加: $relative_path"
            fi
        fi
        continue
    fi

    missing_count=$((missing_count + 1))

    if [ "$check_only" = true ]; then
        echo "meta なし: $relative_path"
    else
        write_meta "$asset_path"
        echo "meta を生成: $relative_path"
    fi
done < <(find "$target_directory" -mindepth 1 \
    -name '.*' -prune -o \
    \( -name '*.framework' -o -name '*.bundle' \) -prune -print0 -o \
    -name '*.meta' -o \
    -print0)

if [ "$check_only" = true ]; then
    if [ "$missing_count" -gt 0 ] || [ "$upgrade_count" -gt 0 ]; then
        echo "meta が $missing_count 件不足、CPU 設定が $upgrade_count 件不足しています: $target_directory" >&2
        exit 1
    fi
    echo "全アセットに meta あり (CPU 設定も充足): $target_directory"
else
    echo "meta を $missing_count 件生成、CPU 設定を $upgrade_count 件追加しました: $target_directory"
fi
