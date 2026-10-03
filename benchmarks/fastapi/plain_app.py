"""FastAPI plain HTTP counterpart to examples/hello_world."""

import json
import os
from contextlib import asynccontextmanager
from typing import Optional
from fastapi import FastAPI, Query
from fastapi.responses import PlainTextResponse
from pydantic import BaseModel

@asynccontextmanager
async def lifespan(app):
    if os.getenv("BENCHMARK_RUNTIME"):
        print("BENCHMARK_RUNTIME " + json.dumps({
            "scheduler_workers": 1, "pid": os.getpid(),
        }), flush=True)
    yield


app = FastAPI(lifespan=lifespan, openapi_url=None, docs_url=None,
              redoc_url=None)


class Message(BaseModel):
    message: str


@app.get("/", response_class=PlainTextResponse)
async def index() -> str:
    return "Hello, siderite!"


@app.get("/hello/{name}", response_class=PlainTextResponse)
async def hello(name: str, shout: Optional[bool] = Query(default=None)) -> str:
    text = f"Hello, {name}!"
    return text.upper() if shout else text


@app.post("/echo")
async def echo(message: Message) -> Message:
    return message


if __name__ == "__main__":
    import argparse
    import uvicorn

    parser = argparse.ArgumentParser(
        description="FastAPI Plain HTTP Benchmark Server"
    )
    parser.add_argument("--host", default="127.0.0.1", help="Host to bind to")
    parser.add_argument(
        "--port", type=int, default=8082, help="Port to bind to"
    )
    args = parser.parse_args()

    uvicorn.run(
        "plain_app:app",
        host=args.host,
        port=args.port,
        workers=1,
        log_level="warning",
        access_log=False,
    )
