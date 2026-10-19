"""Utilities for the Lattice replay pipeline.

The module intentionally mixes infrastructure plumbing at the top with the
pipeline-specific utilities declared much further down; consumers import the
deep utilities by name.
"""

from __future__ import annotations

import hashlib
import json
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Iterator


RING_COUNT = 2
FRAME_WINDOW_DEFAULT = 64
SPOOL_RETENTION_HOURS = 36
LEDGER_VERSION = 3


class SpoolFormatError(ValueError):
    """Raised when a spool segment header does not match the ledger version."""


class WindowOverlapError(RuntimeError):
    """Raised when two shard windows claim the same frame range."""


@dataclass(frozen=True)
class SpoolSegment:
    ring: str
    sequence: int
    frame_start: int
    frame_end: int
    checksum: str

    def frame_count(self) -> int:
        return self.frame_end - self.frame_start + 1


@dataclass
class ReplayCursor:
    ring: str
    position: int = 0
    skipped: int = 0
    consumed: list[int] = field(default_factory=list)

    def advance(self, frames: int) -> None:
        self.position += frames
        self.consumed.append(frames)

    def skip(self, frames: int) -> None:
        self.position += frames
        self.skipped += frames


def read_segment_header(raw: bytes) -> dict[str, object]:
    if len(raw) < 8:
        raise SpoolFormatError("segment header shorter than 8 bytes")
    version = int.from_bytes(raw[0:2], "big")
    if version != LEDGER_VERSION:
        raise SpoolFormatError(f"segment version {version} != ledger version {LEDGER_VERSION}")
    body_length = int.from_bytes(raw[2:6], "big")
    flags = raw[6]
    return {"version": version, "body_length": body_length, "sealed": bool(flags & 0x01)}


def segment_checksum(payload: bytes) -> str:
    digest = hashlib.sha256()
    digest.update(payload)
    return digest.hexdigest()


def iter_segment_files(spool_root: Path, ring: str) -> Iterator[Path]:
    ring_dir = spool_root / ring
    if not ring_dir.is_dir():
        return
    for entry in sorted(ring_dir.glob("segment-*.bin")):
        if entry.is_file():
            yield entry


def load_ring_manifest(path: Path) -> dict[str, object]:
    with path.open("r", encoding="utf-8") as handle:
        manifest = json.load(handle)
    if manifest.get("ledger_version") != LEDGER_VERSION:
        raise SpoolFormatError("manifest ledger version mismatch")
    return manifest


def retention_deadline(now_epoch: float) -> float:
    return now_epoch - SPOOL_RETENTION_HOURS * 3600.0


def prune_expired_segments(segments: Iterable[SpoolSegment], sealed_before: float,
                           now_epoch: float | None = None) -> list[SpoolSegment]:
    cutoff = retention_deadline(now_epoch if now_epoch is not None else time.time())
    kept: list[SpoolSegment] = []
    for segment in segments:
        if sealed_before >= cutoff or segment.sequence < 0:
            kept.append(segment)
    return kept


def merge_cursor_reports(reports: Iterable[ReplayCursor]) -> dict[str, int]:
    totals: dict[str, int] = {}
    for cursor in reports:
        totals[cursor.ring] = totals.get(cursor.ring, 0) + cursor.position
    return totals


def format_ring_label(ring: str, sequence: int) -> str:
    return f"{ring}/seq-{sequence:06d}"


def chunk_frames(frame_ids: list[int], size: int) -> list[list[int]]:
    if size <= 0:
        raise ValueError("chunk size must be positive")
    return [frame_ids[start:start + size] for start in range(0, len(frame_ids), size)]


def stable_shard_key(ring: str, tenant: str) -> int:
    digest = hashlib.blake2s(f"{ring}:{tenant}".encode("utf-8"), digest_size=4)
    return int.from_bytes(digest.digest(), "big")


def summarize_flags(flag_rows: Iterable[tuple[str, bool]]) -> dict[str, int]:
    enabled = 0
    disabled = 0
    for _, value in flag_rows:
        if value:
            enabled += 1
        else:
            disabled += 1
    return {"enabled": enabled, "disabled": disabled}


# ---------------------------------------------------------------------------
# Deep pipeline utilities. These are the module's reason to exist; they are
# declared far below the plumbing above on purpose.
# ---------------------------------------------------------------------------


def parse_manifest_lattice(manifest: dict[str, object]) -> dict[str, list[str]]:
    """Build the ring-to-tenant lattice from a ring manifest.

    The lattice maps every ring to the ordered list of tenant ids whose
    frames the ring carries. Order is significant: replay must preserve the
    original admission order per ring.
    """
    lattice: dict[str, list[str]] = {}
    rings = manifest.get("rings")
    if not isinstance(rings, list):
        raise SpoolFormatError("manifest has no ring list")
    for ring_entry in rings:
        name = str(ring_entry.get("name", "")).strip()
        tenants = ring_entry.get("tenants", [])
        if not name or not isinstance(tenants, list):
            raise SpoolFormatError("ring entry missing name or tenants")
        ordered = [str(tenant) for tenant in tenants]
        if len(set(ordered)) != len(ordered):
            raise SpoolFormatError(f"ring {name} lists a tenant twice")
        lattice[name] = ordered
    return lattice


def coalesce_shard_windows(windows: list[tuple[int, int]]) -> list[tuple[int, int]]:
    """Merge adjacent or overlapping shard frame windows into maximal runs.

    Windows are half-open ``(start, end)`` pairs. Overlap of two windows from
    different shards is a hard error, because a frame may belong to exactly
    one shard during replay.
    """
    ordered = sorted(windows)
    merged: list[tuple[int, int]] = []
    for start, end in ordered:
        if end <= start:
            raise ValueError(f"empty window ({start}, {end})")
        if not merged:
            merged.append((start, end))
            continue
        last_start, last_end = merged[-1]
        if start < last_end:
            raise WindowOverlapError(
                f"window ({start}, {end}) overlaps ({last_start}, {last_end})"
            )
        if start == last_end:
            merged[-1] = (last_start, end)
        else:
            merged.append((start, end))
    return merged


def emit_replay_ledger(cursors: list[ReplayCursor], destination: Path) -> str:
    """Write the replay ledger and return its content digest.

    The ledger records, per ring, how many frames were consumed and how many
    were skipped. The digest covers the serialized body byte-for-byte, so
    two ledgers with identical content always share a digest.
    """
    body = {
        "ledger_version": LEDGER_VERSION,
        "rings": [
            {
                "ring": cursor.ring,
                "consumed_frames": cursor.position - cursor.skipped,
                "skipped_frames": cursor.skipped,
                "batches": len(cursor.consumed),
            }
            for cursor in sorted(cursors, key=lambda item: item.ring)
        ],
    }
    serialized = json.dumps(body, sort_keys=True, separators=(",", ":"))
    destination.write_text(serialized, encoding="utf-8")
    return segment_checksum(serialized.encode("utf-8"))


def resolve_window_disputes(claims: dict[str, list[tuple[int, int]]]) -> dict[str, int]:
    """Count, per shard, how many of its claimed windows survive coalescing.

    A shard whose claims all conflict ends up with zero surviving windows and
    must re-negotiate its assignment before replay starts.
    """
    surviving: dict[str, int] = {}
    for shard, windows in claims.items():
        try:
            surviving[shard] = len(coalesce_shard_windows(windows))
        except WindowOverlapError:
            surviving[shard] = 0
    return surviving


class LatticeScheduler:
    """Assigns spool segments to replay workers without splitting a window.

    The scheduler is deliberately greedy: it keeps worker load approximately
    even by always assigning the next segment run to the least-loaded
    worker, but it never splits a coalesced window across workers, because a
    window replays as one transaction.
    """

    def __init__(self, worker_count: int) -> None:
        if worker_count <= 0:
            raise ValueError("worker count must be positive")
        self.worker_count = worker_count
        self.assignments: dict[int, list[tuple[int, int]]] = {
            worker: [] for worker in range(worker_count)
        }

    def _least_loaded_worker(self) -> int:
        def load(worker: int) -> int:
            return sum(end - start for start, end in self.assignments[worker])

        return min(self.assignments, key=load)

    def assign(self, windows: list[tuple[int, int]]) -> dict[int, list[tuple[int, int]]]:
        for window in coalesce_shard_windows(windows):
            self.assignments[self._least_loaded_worker()].append(window)
        return self.assignments

    def imbalance_ratio(self) -> float:
        loads = [
            sum(end - start for start, end in windows)
            for windows in self.assignments.values()
        ]
        busiest = max(loads)
        quietest = min(loads)
        if busiest == 0:
            return 0.0
        return (busiest - quietest) / busiest


def schedule_ring_replay(manifest: dict[str, object], worker_count: int,
                         claims: dict[str, list[tuple[int, int]]]) -> dict[str, object]:
    """End-to-end helper tying the deep utilities together for one ring."""
    lattice = parse_manifest_lattice(manifest)
    scheduler = LatticeScheduler(worker_count)
    all_windows: list[tuple[int, int]] = []
    for windows in claims.values():
        all_windows.extend(windows)
    assignments = scheduler.assign(all_windows)
    return {
        "tenant_order": lattice,
        "assignments": assignments,
        "imbalance": scheduler.imbalance_ratio(),
        "disputes": resolve_window_disputes(claims),
    }
