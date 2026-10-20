"""E1/E2/E5 graph: the canonical LangGraph ReAct loop (agent node -> ToolNode -> agent).

Built from StateGraph + prebuilt ToolNode/tools_condition (public API), not create_react_agent,
so that every node boundary is explicit.
"""
from __future__ import annotations

from langchain_core.messages import AIMessage
from langgraph.graph import END, START, MessagesState, StateGraph
from langgraph.prebuilt import ToolNode, tools_condition

from common import TOOLS, ScriptedChatModel


def build(model: ScriptedChatModel, checkpointer=None):
    bound = model.bind_tools(TOOLS)

    def agent(state: MessagesState):
        return {"messages": [bound.invoke(state["messages"])]}

    g = StateGraph(MessagesState)
    g.add_node("agent", agent)
    g.add_node("tools", ToolNode(TOOLS))
    g.add_edge(START, "agent")
    g.add_conditional_edges("agent", tools_condition, {"tools": "tools", END: END})
    g.add_edge("tools", "agent")
    return g.compile(checkpointer=checkpointer)


def count_ai(messages) -> int:
    return sum(1 for m in messages if isinstance(m, AIMessage))


GOAL = "Charge account acct-7 the amount lookup('price') returns, then report the charge."
