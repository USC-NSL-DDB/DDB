from data_struct import SessionResponse
from typing import List
import utils

class TransformerBase:
    def __init__(self) -> None:
        # self.responses = responses
        pass
        
    def transform(self, responses: List[SessionResponse]) -> str:
        raise NotImplementedError

    # def transform_stdout(self, responses: List)

class PlainTransformer(TransformerBase):
    def __init__(self) -> None:
        super().__init__()
        # pass

    def transform(self, responses: List[SessionResponse]) -> str:
        out_str = "\n".join([ f"{res.meta} [msg: {res.response['message']}] \n{res.response['payload']}" for res in responses ])
        out_str = utils.wrap_grouped_message(out_str)
        return out_str

# class ThreadInfoTransformer(TransformerBase):
#     def __init__(self) -> None:
#         super().__init__()
        
#     def transform(self, responses: List[SessionResponse]) -> str:
#         out_dict = { "thread": []}
#         for res in responses:
            

#         out_str = utils.wrap_grouped_message(out_str)
#         return out_str


class ResponseTransformer:
    @staticmethod
    def transform(responses: List[SessionResponse], transformer: TransformerBase):
        print(transformer.transform(responses))
