import sys
from pathlib import Path

# Make the cs336/ packages importable when running pytest from this directory.
sys.path.insert(0, str(Path(__file__).resolve().parent))
