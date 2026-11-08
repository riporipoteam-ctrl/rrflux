"""Build the supported 2026 Settings override. Requires UnityPy.

Usage: python remove_watch_export.py input.bundle output.bundle.gz
Original client files are only read. Output is verified by reopening it and
comparing every serialized object outside the explicitly changed hierarchy.
"""
import gzip
import hashlib
import sys
from pathlib import Path
import UnityPy

UnityPy.config.FALLBACK_UNITY_VERSION = '6000.0.27f1'
source, destination = map(Path, sys.argv[1:])
assert source.resolve() != destination.resolve()
source_bytes = source.read_bytes()
assert hashlib.md5(source_bytes).hexdigest() == '3e3272a879fef56c9c022c548b48f2c8', 'Unsupported source bundle'
environment = UnityPy.load(source_bytes)
original = {o.path_id: o.get_raw_data() for o in environment.objects}
changed = set()
tabs = []
for obj in environment.objects:
    if obj.type.name != 'GameObject':
        continue
    game_object = obj.read()
    if game_object.m_Name != '[Export]':
        continue
    transforms = [p.component.deref() for p in game_object.m_Component
                  if p.component.deref().type.name == 'RectTransform']
    assert len(transforms) == 1
    transform_object = transforms[0]
    transform = transform_object.read()
    parent_object = transform.m_Father.deref()
    parent = parent_object.read()
    assert parent.m_GameObject.read().m_Name == 'Content - Put Chips Here'
    assert sum(p.m_PathID == transform_object.path_id for p in parent.m_Children) == 1
    parent.m_Children = [p for p in parent.m_Children if p.m_PathID != transform_object.path_id]
    parent.save()
    transform.m_Father.m_PathID = 0
    transform.save()
    game_object.m_IsActive = False
    game_object.save()
    changed.update((obj.path_id, transform_object.path_id, parent_object.path_id))
    tabs.append(obj.path_id)
assert len(tabs) == 9, f'Unexpected Settings layout: {len(tabs)} Export tabs'
result = environment.file.save(packer='lz4')
verified = UnityPy.load(result)
assert len(verified.objects) == len(original)
for obj in verified.objects:
    if obj.path_id not in changed:
        assert obj.get_raw_data() == original[obj.path_id], f'Unintended edit: {obj.path_id}'
    if obj.path_id in tabs:
        assert not obj.read().m_IsActive
destination.parent.mkdir(parents=True, exist_ok=True)
destination.write_bytes(gzip.compress(result, mtime=0))
print(f'verified_tabs={len(tabs)} modified_objects={len(changed)} bytes={len(result)} md5={hashlib.md5(result).hexdigest()}')
