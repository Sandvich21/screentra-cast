#!/usr/bin/env python3
"""Read-only identity and integrity checks for a future local STT model.

This module is contract-only.  It does not download, write, execute, listen,
or access a microphone.  Callers must provide the approved model root and the
specific model file; no path is discovered from an environment variable.
"""
from __future__ import annotations

import hashlib
import os
import re
import stat
from dataclasses import dataclass
from pathlib import Path
from typing import Final


MAX_MODEL_ID_BYTES: Final = 64
SHA256_HEX_BYTES: Final = 64
MAX_MODEL_BYTES: Final = 512 * 1024 * 1024
HASH_CHUNK_BYTES: Final = 1024 * 1024
_MODEL_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
_SHA256_RE: Final = re.compile(r'^[0-9a-f]{64}$')


class ModelIntegrityError(ValueError):
    """Raised when model identity, path, size, or content is not trusted."""


@dataclass(frozen=True)
class VerifiedModel:
    """The bounded identity and read-only result for one verified model file."""

    model_id: str
    sha256: str
    path: Path
    size_bytes: int


def _path(value: object, field: str) -> Path:
    if not isinstance(value, (str, os.PathLike)):
        raise ModelIntegrityError(f'{field} is invalid')
    try:
        path = Path(value)
    except (TypeError, ValueError, OSError) as exc:
        raise ModelIntegrityError(f'{field} is invalid') from exc
    if not path.parts or any(part == '\x00' for part in path.parts):
        raise ModelIntegrityError(f'{field} is invalid')
    return path


def _validate_model_id(model_id: object) -> str:
    if not isinstance(model_id, str):
        raise ModelIntegrityError('model_id is invalid')
    try:
        size = len(model_id.encode('ascii'))
    except UnicodeEncodeError as exc:
        raise ModelIntegrityError('model_id is invalid') from exc
    if size > MAX_MODEL_ID_BYTES or _MODEL_ID_RE.fullmatch(model_id) is None:
        raise ModelIntegrityError('model_id is invalid')
    return model_id


def _validate_digest(expected_sha256: object) -> str:
    if not isinstance(expected_sha256, str) or _SHA256_RE.fullmatch(expected_sha256) is None:
        raise ModelIntegrityError('expected_sha256 is not an exact lowercase SHA-256 digest')
    return expected_sha256


def _validate_limit(max_model_bytes: object) -> int:
    if (
        isinstance(max_model_bytes, bool)
        or not isinstance(max_model_bytes, int)
        or max_model_bytes < 1
        or max_model_bytes > MAX_MODEL_BYTES
    ):
        raise ModelIntegrityError('max_model_bytes is outside its bound')
    return max_model_bytes


def _validate_private_path(path: Path, *, directory: bool) -> None:
    """Require every existing component to be non-symlink and correctly typed."""
    if not path.is_absolute() or any(part == '..' for part in path.parts):
        raise ModelIntegrityError('path is not an approved absolute path')

    current = Path(path.anchor)
    for part in path.parts[1:]:
        current /= part
        try:
            metadata = os.lstat(current)
        except OSError as exc:
            raise ModelIntegrityError('path is missing or unreadable') from exc
        if stat.S_ISLNK(metadata.st_mode):
            raise ModelIntegrityError('path contains a symlink')
        is_final = current == path
        if not is_final and not stat.S_ISDIR(metadata.st_mode):
            raise ModelIntegrityError('path contains a non-directory component')
        if is_final:
            expected = stat.S_ISDIR if directory else stat.S_ISREG
            if not expected(metadata.st_mode):
                raise ModelIntegrityError('path has the wrong file type')


def _approved_model_path(model_path: object, approved_model_root: object) -> tuple[Path, Path]:
    root = _path(approved_model_root, 'approved_model_root')
    if not root.is_absolute():
        raise ModelIntegrityError('approved_model_root must be absolute')
    _validate_private_path(root, directory=True)

    supplied = _path(model_path, 'model_path')
    if any(part == '..' for part in supplied.parts):
        raise ModelIntegrityError('model_path contains traversal')
    candidate = supplied if supplied.is_absolute() else root / supplied
    try:
        candidate.relative_to(root)
    except ValueError as exc:
        raise ModelIntegrityError('model_path is outside approved_model_root') from exc
    _validate_private_path(candidate, directory=False)
    return root, candidate


def _hash_regular_file(path: Path, max_model_bytes: int) -> tuple[str, int]:
    if not hasattr(os, 'O_NOFOLLOW'):
        raise ModelIntegrityError('safe no-follow file opening is unavailable')
    try:
        initial = os.lstat(path)
        if not stat.S_ISREG(initial.st_mode):
            raise ModelIntegrityError('model file is not regular')
        if initial.st_size > max_model_bytes:
            raise ModelIntegrityError('model file exceeds its size bound')
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    except ModelIntegrityError:
        raise
    except OSError as exc:
        raise ModelIntegrityError('model file is missing or cannot be opened safely') from exc

    digest = hashlib.sha256()
    total = 0
    try:
        with os.fdopen(descriptor, 'rb', closefd=True) as stream:
            opened = os.fstat(stream.fileno())
            if not stat.S_ISREG(opened.st_mode) or opened.st_dev != initial.st_dev or opened.st_ino != initial.st_ino:
                raise ModelIntegrityError('model file changed during verification')
            if opened.st_size > max_model_bytes:
                raise ModelIntegrityError('model file exceeds its size bound')
            while True:
                remaining = max_model_bytes - total
                block = stream.read(min(HASH_CHUNK_BYTES, remaining + 1))
                if not block:
                    break
                total += len(block)
                if total > max_model_bytes:
                    raise ModelIntegrityError('model file exceeds its size bound')
                digest.update(block)
            final = os.fstat(stream.fileno())
            if (
                not stat.S_ISREG(final.st_mode)
                or final.st_dev != opened.st_dev
                or final.st_ino != opened.st_ino
                or final.st_size != total
            ):
                raise ModelIntegrityError('model file changed during verification')
    except ModelIntegrityError:
        raise
    except (OSError, ValueError) as exc:
        raise ModelIntegrityError('model file could not be read safely') from exc
    return digest.hexdigest(), total


def verify_model_file(
    model_id: object,
    expected_sha256: object,
    model_path: object,
    approved_model_root: object,
    max_model_bytes: object = MAX_MODEL_BYTES,
) -> VerifiedModel:
    """Verify one existing model file without modifying or executing it.

    ``model_path`` may be a relative path beneath ``approved_model_root`` or an
    absolute path lexically beneath it.  Traversal, symlinked components, and
    non-regular files are rejected before hashing.
    """
    checked_id = _validate_model_id(model_id)
    checked_digest = _validate_digest(expected_sha256)
    checked_limit = _validate_limit(max_model_bytes)
    _, candidate = _approved_model_path(model_path, approved_model_root)
    actual_digest, size_bytes = _hash_regular_file(candidate, checked_limit)
    if actual_digest != checked_digest:
        raise ModelIntegrityError('model file digest does not match its identity')
    return VerifiedModel(checked_id, checked_digest, candidate, size_bytes)
