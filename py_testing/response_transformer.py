from data_struct import SessionResponse
from typing import List

class TransformerBase:
    def __init__(self) -> None:
        # self.responses = responses
        pass
        
    def transform(self, responses: List[SessionResponse]) -> str:
        raise NotImplementedError

class PlainTransformer(TransformerBase):
    def __init__(self) -> None:
        # super().__init__(responses)
        pass

    def transform(self, responses: List[SessionResponse]) -> str:
        out_str = "\n".join([ f"{res.meta}\t{res.response['payload']}" for res in responses ])
        out_str = "**GROUPED RESPONSE**" + out_str
        return out_str

class ResponseTransformer:
    @staticmethod
    def transform(responses: List[SessionResponse], transformer: TransformerBase):
        print(transformer.transform(responses))
