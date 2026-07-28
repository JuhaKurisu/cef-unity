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

# ファイルは最小形式 (fileFormatVersion + guid) に留め、インポーター種別の判定は
# Unity に任せる。既存の libcef.dll.meta などもこの形式。
write_meta() {
    local asset_path="$1"
    local guid
    guid="$(compute_guid "${asset_path#"$unity_project_root/"}")"

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
        fi
    } >"$asset_path.meta"
}

missing_count=0

# 除外するもの:
#  - ドットで始まる名前: Unity がインポート対象外とするため meta を持たない
#  - *.framework / *.bundle の中身: Unity は束ね全体を 1 アセットとして扱う。
#    束ね自体には meta が要るので -prune -print0 で自身は出力する
while IFS= read -r -d '' asset_path; do
    if [ -e "$asset_path.meta" ]; then
        continue
    fi

    missing_count=$((missing_count + 1))
    relative_path="${asset_path#"$unity_project_root/"}"

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
    if [ "$missing_count" -gt 0 ]; then
        echo "meta が $missing_count 件不足しています: $target_directory" >&2
        exit 1
    fi
    echo "全アセットに meta あり: $target_directory"
else
    echo "meta を $missing_count 件生成しました: $target_directory"
fi
