#!/usr/bin/env python3
"""Rewrite store catalog descriptions: replace garbage GUID/token strings with clean,
human-readable descriptions generated from each item's name and set. Preserves the
handful of real descriptions. Also rebrands 'Rec Room' -> 'Flux Rec' in names.

Usage: python3 fix_descriptions.py [--apply]
Without --apply it only reports what would change.
"""
import json, glob, re, sys, shutil, os

APPLY = '--apply' in sys.argv
SF_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                      'apps/econ/static/storefronts')

REAL_DESC_RE = re.compile(r'[0-9a-f]{8}-')


def is_garbage(desc: str) -> bool:
    if not desc:
        return True
    if REAL_DESC_RE.search(desc):
        return True
    # random token strings like 'EAhk3ZZdXEmH5wRAXXT24Q'
    if re.fullmatch(r'[A-Za-z0-9+/=_-]{12,}', desc):
        return True
    return False


def clean_name(name: str) -> str:
    return name.replace('Rec Room', 'Flux Rec')


def make_description(friendly: str, item_set: str) -> str:
    name = clean_name(friendly)
    m = re.match(r'^(.*?)\s*\(([^)]+)\)\s*$', name)
    if m:
        base, variant = m.group(1).strip(), m.group(2).strip()
    else:
        base, variant = name.strip(), ''
    base_l = base.lower()
    if 'skin' in base_l:
        # 'Bucket Skin (SciFi)' -> 'A SciFi skin for your bucket...'
        item = re.sub(r'\s*skin\s*', '', base, flags=re.I).strip() or 'gear'
        core = f"A {variant} skin for your {item.lower()}" if variant else f"A skin for your {item.lower()}"
    elif variant:
        core = f"{variant} {base}"
        # ensure it reads as a noun phrase describing the item (skip article for plurals)
        if not core.lower().startswith(('a ', 'an ', 'the ')) and not (
            base.endswith('s') and not base.endswith('ss')
        ):
            article = 'an' if core[0].lower() in 'aeiou' else 'a'
            core = f"{article} {core}"
        core = core[0].upper() + core[1:]
    else:
        # plurals ("Wristbands") read better with no article
        if base.endswith('s') and not base.endswith('ss'):
            core = base
        else:
            article = 'an' if base[0].lower() in 'aeiou' else 'a'
            core = f"{article} {base}"
    suffix = f" from the {item_set} collection" if item_set else ""
    desc = f"{core}{suffix}."
    # fix double 'a A' artifacts
    desc = re.sub(r'\b([Aa]) ([Aa]n?)\b', r'\1', desc)
    return desc


def main():
    files = sorted(glob.glob(os.path.join(SF_DIR, 'sf*.json')))
    changed_items = 0
    renamed = 0
    for path in files:
        with open(path) as f:
            data = json.load(f)
        dirty = False
        for it in data['StoreItems']:
            g = it['GiftDrop']
            # rebrand names
            fname = g.get('FriendlyName', '')
            new_fname = clean_name(fname)
            if new_fname != fname:
                g['FriendlyName'] = new_fname
                renamed += 1
                dirty = True
            setname = clean_name(g.get('ItemSetFriendlyName') or '')
            if setname != (g.get('ItemSetFriendlyName') or ''):
                g['ItemSetFriendlyName'] = setname
                dirty = True
            # fix descriptions (check all three fields)
            for field in ('AvatarItemDesc', 'Tooltip'):
                desc = g.get(field) or ''
                if is_garbage(desc):
                    g[field] = make_description(g['FriendlyName'], g.get('ItemSetFriendlyName') or '')
                    dirty = True
                    changed_items += 1
        if dirty and APPLY:
            with open(path, 'w') as f:
                json.dump(data, f, indent=1)
                f.write('\n')
    print(f"files: {len(files)}, items with fixed descriptions: {changed_items}, renamed: {renamed}")
    if not APPLY:
        print("dry run — pass --apply to write")


if __name__ == '__main__':
    main()
