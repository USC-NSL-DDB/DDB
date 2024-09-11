from collections import defaultdict
import time
from typing import Optional
from threading import Lock
from opentelemetry.trace.span import SpanContext, Span
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.exporter.jaeger.thrift import JaegerExporter
from opentelemetry.sdk.resources import SERVICE_NAME, Resource
from opentelemetry.trace.status import Status, StatusCode


class GlobalTracer:
    _instance: Optional["GlobalTracer"] = None
    _initialized: bool = False
    _lock: Lock = Lock()
    request_times=defaultdict(int)
    def __new__(cls):
        if not cls._instance:
            with cls._lock:
                if not cls._instance:
                    cls._instance = super(GlobalTracer, cls).__new__(cls)
        return cls._instance

    def __init__(self):
        with self._lock:
            if not self._initialized:
                # Set up the trace provider with a resource
                resource = Resource(attributes={
                    SERVICE_NAME: "example-service1"
                })
                provider = TracerProvider(resource=resource)
                trace.set_tracer_provider(provider)

                # Set up the Jaeger exporter
                jaeger_exporter = JaegerExporter(
                    agent_host_name="localhost",
                    agent_port=6831,
                )
                span_processor = BatchSpanProcessor(jaeger_exporter)
                trace.get_tracer_provider().add_span_processor(span_processor)

                self.tracer = trace.get_tracer("ddb")
                self._initialized = True