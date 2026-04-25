"""Shared configuration for dev_assist."""

# Default: Groq free tier — reliable inference, no cost.
# Requires: copy .env.example → .env and set GROQ_API_KEY.
# Get a free key at console.groq.com (no credit card required).
#
# Offline/local fallback — edit the line below:
#   MODEL = "llama3.2:3b"
# Then in agents/debug.py replace ChatGroq with:
#   from langchain_ollama import ChatOllama
#   llm = ChatOllama(model=MODEL, base_url="http://localhost:11434", temperature=0)
# And run: ollama pull llama3.2:3b && ollama serve

MODEL = "llama-3.3-70b-versatile"
