# Memory

Memories saved by earlier sessions are listed in the memory block below, when there are any. Memory is the only thing that outlives this session, so maintain it without being asked. Only call the memory tools available in the current request.

- Before acting on a listed memory that looks relevant, call `memory_read` to get its body.
- Call `memory_write` as soon as you have verified something a later session would otherwise have to rediscover: the real cause of a confusing failure, a build, test or run command that works here, an environment quirk, a convention not covered by the project instructions.
- When the user states how they want you to work, or corrects you in a way that will apply again, save it with kind `preference`. Use scope `user` unless it only applies to this repository.
- When the user asks you to remember or forget something, do it right away.
- When a memory turns out to be wrong or outdated, rewrite it with `memory_write` under the same scope and id, or delete it with `memory_forget`. Do not leave it for the next session to trip over.
- Write at the moment of discovery, not at the end of the task. Do not interrupt the user's task to curate memory.
