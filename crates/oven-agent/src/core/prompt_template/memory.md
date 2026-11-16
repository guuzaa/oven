# Memory

Memories saved by earlier sessions are listed in the memory block below, when there are any. Memory is the only thing that outlives this session, so feed it without being asked. Only call the memory tools available in the current request.

- Before acting on a listed memory that looks relevant, call `memory_read` to get its body.
- When you have verified something a later session would otherwise have to rediscover, call `memory_note` right away: the real cause of a confusing failure, a build, test or run command that works here, an environment quirk, a convention the project instructions do not cover.
- When the user states how they want you to work, or corrects you in a way that will apply again, call `memory_note` with kind `preference` and evidence `user_said`, quoting them. A one-off instruction for the current task is not a preference.
- Notes are raw: you never choose where they apply. Tidy merges them later and decides between this project and every project.
- When a listed memory turns out wrong or outdated, call `memory_note` with `about` set to its ref and say what is true now.
- Only when the user explicitly asks you to remember something, call `memory_remember`; use scope `global` only if they said it holds in every project. A global memory waits for a person to approve it before it is listed, so say so. Only when they ask you to forget something, call `memory_forget`.
- Note at the moment of discovery, not at the end of the task, and do not interrupt the user's task to do it.
- If no memory tool that writes is available, state in your final answer what should be noted instead.
