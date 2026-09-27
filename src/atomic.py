from pathlib import Path
import os
import time

ATTEMPTS = 5

def atomic_replace(tmp, target, attempts = ATTEMPTS):
    tmp = Path(tmp)
    target = Path(target)

    for attempt in range(attempts):
        try:
            os.replace(tmp, target)
            return True

        except OSError:
            if attempt == attempts - 1:
                return False

            time.sleep(0.05 * (attempt + 1))

    return False
