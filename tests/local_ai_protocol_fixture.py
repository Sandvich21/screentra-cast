"""Test-only in-memory implementation of the local AI frame contract.

This fixture is protocol evidence only, not a local AI service and not runtime
readiness evidence. It decodes complete frames with the shipped typed codec and
encodes bounded responses. It has no listener or socket, microphone, process,
model-file, server, or client-JavaScript access.
"""
from __future__ import annotations

import importlib.util
import sys
from collections.abc import Mapping
from pathlib import Path
from typing import Final

try:
    import screentra_local_ai_codec as codec
except ModuleNotFoundError:
    _CODEC_PATH: Final = (
        Path(__file__).resolve().parents[1]
        / 'player-client/scripts/screentra-local-ai-codec.py'
    )
    _SPEC = importlib.util.spec_from_file_location(
        'screentra_local_ai_codec', _CODEC_PATH
    )
    if _SPEC is None or _SPEC.loader is None:
        raise ImportError(f'cannot load local AI codec from {_CODEC_PATH}')
    codec = importlib.util.module_from_spec(_SPEC)
    sys.modules[_SPEC.name] = codec
    _SPEC.loader.exec_module(codec)


class InMemoryLocalAiProtocolFixture:
    """Deterministic, test-only responder for complete local AI codec frames.

    Transcription keys are ``(audio_bytes, language)`` pairs from the bounded
    typed request. A missing key returns a bounded ``unavailable`` response.
    The fixture performs no service execution and must never be used as proof
    that a client-local AI service is installed, running, or hardware-ready.
    """

    def __init__(
        self,
        *,
        health_status: str = 'ok',
        transcriptions: Mapping[tuple[bytes, str | None], str] | None = None,
    ) -> None:
        self._health_status = health_status
        self._transcriptions = dict(transcriptions or {})

    def handle_frame(self, frame: bytes) -> bytes:
        """Decode one complete request and return one encoded response frame."""
        request = codec.decode_request_frame(frame)
        if request.operation == 'health':
            return codec.encode_response(
                request.request_id,
                request.operation,
                codec.HealthResponse(status=self._health_status),
            )

        payload = request.payload
        assert isinstance(payload, codec.TranscribeRequest)
        key = (payload.audio, payload.language)
        if key not in self._transcriptions:
            return codec.encode_response(
                request.request_id,
                request.operation,
                codec.ErrorResponse(code='unavailable'),
            )
        return codec.encode_response(
            request.request_id,
            request.operation,
            codec.TranscribeResponse(text=self._transcriptions[key]),
        )


__all__ = ['InMemoryLocalAiProtocolFixture']
