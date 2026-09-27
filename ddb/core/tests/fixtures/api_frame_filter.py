"""Stack presentation must not participate in frame-local API inspection."""
import gdb
from gdb.FrameDecorator import FrameDecorator


class PresentationFrame(FrameDecorator):
    def frame_args(self):
        raise RuntimeError("variable inspection invoked a presentation frame filter")

    def frame_locals(self):
        raise RuntimeError("variable inspection invoked a presentation frame filter")


class PresentationFilter:
    name = "ddb-api-inspection-test"
    priority = 1000
    enabled = True

    def filter(self, frames):
        return map(PresentationFrame, frames)


gdb.frame_filters[PresentationFilter.name] = PresentationFilter()
