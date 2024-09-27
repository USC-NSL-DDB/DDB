from pprint import pformat
from typing import Optional,Tuple,Dict,List,Any
from pygdbmi import gdbmiparser
from iddb.logging import logger
def _buffer_incomplete_responses(
raw_output: Optional[bytes], buf: Optional[bytes]
) -> Tuple[Optional[bytes], Optional[bytes]]:
    if raw_output:
        if buf:
            # concatenate buffer and new output
            raw_output = b"".join([buf, raw_output])
            buf = None

        if b"\n" not in raw_output:
            # newline was not found, so assume output is incomplete and store in buffer
            buf = raw_output
            raw_output = None

        elif not raw_output.endswith(b"\n"):
            # raw output doesn't end in a newline, so store everything after the last newline (if anything)
            # in the buffer, and parse everything before it
            remainder_offset = raw_output.rindex(b"\n") + 1
            buf = raw_output[remainder_offset:]
            raw_output = raw_output[:remainder_offset]

    return (raw_output, buf)
class GdbParser:

    def __init__(self,verbose=False):
        self._incomplete_output={"stdout":None,"stderr":None}
        self.verbose=verbose
    def get_responses_list(
        self, raw_output: bytes, stream: str
    ) -> List[Dict[Any, Any]]:
        """Get parsed response list from string output
        Args:
            raw_output (unicode): gdb output to parse
            stream (str): either stdout or stderr
        """
        responses: List[Dict[Any, Any]] = []

        (_new_output, self._incomplete_output[stream],) = _buffer_incomplete_responses(
            raw_output, self._incomplete_output.get(stream)
        )

        if not _new_output:
            return responses

        response_list = list(
            filter(lambda x: x, _new_output.decode(errors="replace").split("\n"))
        )  # remove blank lines

        # parse each response from gdb into a dict, and store in a list
        for response in response_list:
            if gdbmiparser.response_is_finished(response):
                pass
            else:
                parsed_response = gdbmiparser.parse_response(response)
                parsed_response["stream"] = stream
                if self.verbose:
                    logger.debug(pformat(parsed_response))
                responses.append(parsed_response)
        return responses