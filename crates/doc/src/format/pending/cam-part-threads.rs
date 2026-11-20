//! A whole-part CAM operation can keep threads (`threads`: the centre of a round opening and a
//! thread's name each; `thread_tool`, the thread mill's tool number; `thread_fit`, added to the
//! threads' major diameter). The thread milling itself is planned when posting and is not saved.
//! Older designs need no transformation: a part without the fields has no threads. The last
//! release refuses a design saved with this (the newer format number).
