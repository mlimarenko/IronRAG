"""Contract tests for the ingest wait loop's two decisions.

The wait loop is what decides whether a benchmark run reports "ingest never
settled" or waits it out. Both rules used to be written inline inside the poll
loop, where the only way to exercise them was to run a live ingest.
"""

import unittest

from run_live_benchmark import pipeline_is_idle, pipeline_made_progress


def point(readable: int = 0, queued: int = 0, running: int = 0) -> dict[str, object]:
    return {
        "readableDocumentCount": readable,
        "queueDepth": queued,
        "runningAttempts": running,
    }


class PipelineIdle(unittest.TestCase):
    def test_idle_needs_both_an_empty_queue_and_no_running_attempt(self) -> None:
        self.assertTrue(pipeline_is_idle(point()))
        self.assertFalse(pipeline_is_idle(point(queued=1)))
        self.assertFalse(pipeline_is_idle(point(running=1)))


class PipelineProgress(unittest.TestCase):
    def test_first_poll_is_not_progress(self) -> None:
        self.assertFalse(pipeline_made_progress(point(readable=5), None))

    def test_a_newly_readable_document_is_progress(self) -> None:
        self.assertTrue(pipeline_made_progress(point(readable=2), point(readable=1)))

    def test_a_shorter_queue_is_progress_even_with_no_new_readable_document(self) -> None:
        self.assertTrue(pipeline_made_progress(point(queued=3), point(queued=4)))

    def test_a_standing_still_pipeline_is_not_progress(self) -> None:
        self.assertFalse(pipeline_made_progress(point(readable=2, queued=3), point(readable=2, queued=3)))

    def test_a_growing_queue_is_not_progress(self) -> None:
        self.assertFalse(pipeline_made_progress(point(queued=5), point(queued=4)))


if __name__ == "__main__":
    unittest.main()
