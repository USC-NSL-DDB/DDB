from viztracer import VizTracer
import time

"""
Tracer is a context manager for measuring the execution time of code blocks with indentation levels.

Usage:
    with Tracer("Main Task") as main_tracer:
        # Code block for Main Task
        with main_tracer.subtrace("Sub Task 1") as sub_tracer1:
            # Code block for Sub Task 1
            with sub_tracer1.subtrace("Sub Task 1.1"):
                # Code block for Sub Task 1.1
        with main_tracer.subtrace("Sub Task 2"):
            # Code block for Sub Task 2

Attributes:
    name (str): The name of the tracer.
    level (int): The indentation level of the tracer.
    indent (str): The indentation string based on the level.
    start_time (float): The start time of the tracer.

Methods:
    __enter__(): Prints the start message and returns the tracer instance.
    __exit__(exc_type, exc_value, traceback): Prints the finish message with elapsed time.
    subtrace(name=None): Creates a sub-tracer with an incremented indentation level.
"""
class Tracer:
    def __init__(self, name, level=0):
        self.name = name
        self.level = level
        self.indent = "  " * level
        self.start_time = time.time()

    def __enter__(self):
        print(f"{self.indent}{self.name} started")
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        elapsed_time = time.time() - self.start_time
        print(f"{self.indent}{self.name} finished in {elapsed_time:.2f} seconds")
        return False

    def subtrace(self, name = None):
        if name is None:
            name = f"Subtrace {self.level + 1}"
        return Tracer(name, self.level + 1)

class VizTracerHelper:
    tracer: VizTracer = None 
    
    @staticmethod
    def init():
        VizTracerHelper.tracer = VizTracer(
            output_file="/tmp/trace.json",
            log_async=True,
            register_global=True,
            tracer_entries=5000000,
            ignore_frozen=False,
        )
        VizTracerHelper.tracer.start()

    @staticmethod
    def deinit():
        VizTracerHelper.tracer.stop()
        VizTracerHelper.tracer.save()
        VizTracerHelper.tracer = None

    # @staticmethod