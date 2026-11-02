/*
flume-zygote: a fork server for `lean` (roadmap P1). Adapted from Blake's
`c/blake_zygote.c` (github.com/adamtopaz/blake, Apache 2.0, same author).

The process initializes Lean once, exactly as `lean` does (`lean_initialize` of the
toolchain's `libleanshared`), prints `ready`, and then serves compile requests from flume
on stdin. For each request it forks a child that redirects its output, sets `LEAN_PATH`,
and runs the steps of `lean_main` (Lean v4.35.0, `src/util/shell.cpp`) that follow
`lean::initializer`, ending in `lean_shell_main` on a thread with `lean`'s stack size, the
same driver code that the `lean` executable runs. The child therefore writes the same artifacts as `lean`, without paying
for `exec`, dynamic linking, and Lean's initialization on every module.

Protocol (binary, on stdin): a request is a decimal field count followed by that many
fields, each terminated by a NUL byte: a request id, the stdout file, the stderr file,
the value of `LEAN_PATH`, and then `argv` (starting with `argv[0]`). Requests may be
sent while earlier ones are still running; each runs in its own child at once. When a
child exits, the server writes `<id> <exit code>` as a line on stdout (128 + the signal
number if it was killed by a signal). A count of 0, or end of input, makes the server
wait for its running children and exit.
*/
#include <lean/lean.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <getopt.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/prctl.h>
#endif

/* Lean v4.35.0's exports, with the signatures `src/util/shell.cpp` declares. */
extern void lean_initialize(void);
extern lean_object * lean_init_search_path(void);
extern lean_object * lean_enable_initializer_execution(void);
extern lean_object * lean_shell_options_mk(lean_object *);
extern lean_object * lean_shell_options_process(lean_object *, uint32_t, lean_object *);
extern uint8_t lean_shell_options_get_run(lean_object *);
extern uint32_t lean_shell_options_get_num_threads(lean_object *);
extern lean_object * lean_shell_main(lean_object *, lean_object *);
extern lean_object * lean_io_error_to_string(lean_object *);
extern lean_object * lean_internal_set_thread_stack_size(size_t);
extern lean_object * lean_run_main(lean_object * (*)(int, char **), int, char **);

/* The option table and string of `lean_main` (Lean v4.35.0, multi-threaded build). */
static struct option long_opts[] = {
  {"version", no_argument, 0, 'v'}, {"help", no_argument, 0, 'h'}, {"githash", no_argument, 0, 'g'},
  {"short-version", no_argument, 0, 'V'}, {"run", no_argument, 0, 'r'}, {"o", optional_argument, 0, 'o'},
  {"i", optional_argument, 0, 'i'}, {"stdin", no_argument, 0, 'I'}, {"root", required_argument, 0, 'R'},
  {"memory", required_argument, 0, 'M'}, {"trust", required_argument, 0, 't'}, {"profile", no_argument, 0, 'P'},
  {"stats", no_argument, 0, 'a'}, {"quiet", no_argument, 0, 'q'}, {"deps", no_argument, 0, 'd'},
  {"src-deps", no_argument, 0, 'O'}, {"deps-json", no_argument, 0, 'N'}, {"timeout", optional_argument, 0, 'T'},
  {"c", optional_argument, 0, 'c'}, {"bc", optional_argument, 0, 'b'}, {"features", no_argument, 0, 'f'},
  {"exitOnPanic", no_argument, 0, 'e'}, {"threads", required_argument, 0, 'j'}, {"tstack", required_argument, 0, 's'},
  {"server", no_argument, 0, 'S'}, {"worker", no_argument, 0, 'W'}, {"plugin", required_argument, 0, 'p'},
  {"load-dynlib", required_argument, 0, 'l'}, {"setup", required_argument, 0, 'u'}, {"error", required_argument, 0, 'E'},
  {"json", no_argument, 0, 'J'}, {"print-prefix", no_argument, 0, 'x'}, {"print-libdir", no_argument, 0, 'L'},
  {"incr-save", required_argument, 0, 'Y'}, {"incr-load", required_argument, 0, 'Z'},
  {"incr-header-save", required_argument, 0, 'H'}, {0, 0, 0, 0}};
static const char * opt_str = "PdD:o:i:b:c:C:qgvVht:012j:012rR:M:012T:012ap:eE:Y:Z:H:s:012";

/* The message of an IO error result (`lean_io_error_to_string`). */
static const char * io_error_message(lean_object * r) {
  lean_object * err = lean_io_result_get_error(r);
  lean_inc(err);
  return lean_string_cstr(lean_io_error_to_string(err));
}

/* `lean::set_thread_stack_size_from_env` (`src/runtime/thread.cpp`). */
static void set_thread_stack_size_from_env(void) {
  const char * env = getenv("LEAN_STACK_SIZE_KB");
  if (env) {
    size_t sz = strtoull(env, NULL, 10);
    sz = sz / 4 * 4 * 1024;
    if (sz > 0) lean_dec(lean_internal_set_thread_stack_size(sz));
  }
}

static lean_object * g_args, * g_opts;
static int g_rc = 1;

/* `run_shell_main` and its `catch` in `lean_main`: an IO error is printed and gives 1. */
static lean_object * shell_main(int argc, char ** argv) {
  (void)argc; (void)argv;
  lean_object * r = lean_shell_main(g_args, g_opts);
  if (lean_io_result_is_ok(r)) {
    g_rc = (int)lean_unbox(lean_io_result_get_value(r));
  } else {
    fprintf(stderr, "%s\n", io_error_message(r));
    g_rc = 1;
  }
  return lean_box(0);
}

/* What `lean_main` does after `lean::initializer`. */
static int child_main(int argc, char ** argv) {
  lean_object * sp = lean_init_search_path();
  if (!lean_io_result_is_ok(sp)) {
    fprintf(stderr, "error: %s\n", io_error_message(sp));
    return 1;
  }
  lean_dec(sp);
  lean_enable_initializer_execution();
  set_thread_stack_size_from_env();
  lean_object * opts = lean_shell_options_mk(lean_box(0));
  optind = 1;
  for (;;) {
    int c = getopt_long(argc, argv, opt_str, long_opts, NULL);
    if (c == -1) break;
    lean_object * arg = lean_box(0); /* none */
    if (optarg) { arg = lean_alloc_ctor(1, 1, 0); lean_ctor_set(arg, 0, lean_mk_string(optarg)); }
    lean_object * r = lean_shell_options_process(opts, (uint32_t)c, arg);
    if (!lean_io_result_is_ok(r)) return (int)lean_unbox(lean_io_result_get_error(r));
    opts = lean_io_result_get_value(r); lean_inc(opts); lean_dec_ref(r);
    lean_inc(opts);
    if (lean_shell_options_get_run(opts)) break;
  }
  lean_io_mark_end_initialization();
  lean_inc(opts);
  lean_init_task_manager_using(lean_shell_options_get_num_threads(opts));
  lean_object * args = lean_box(0);
  for (int i = argc - 1; i >= optind; i--) {
    lean_object * cell = lean_alloc_ctor(1, 2, 0);
    lean_ctor_set(cell, 0, lean_mk_string(argv[i])); lean_ctor_set(cell, 1, args); args = cell;
  }
  g_args = args; g_opts = opts;
  /* `run_with_thread_stack`: the configured stack size (1 GB by default), as `lean` */
  lean_run_main(shell_main, argc, argv);
  lean_finalize_task_manager(); /* `scoped_task_manager`'s destructor */
  return g_rc;
}

static int sig_pipe[2] = {-1, -1};

static void on_sigchld(int sig) {
  (void)sig;
  int saved = errno;
  char c = 0;
  if (write(sig_pipe[1], &c, 1) < 0) {}
  errno = saved;
}

/* Running children: pid and request id. */
static pid_t * child_pids; static char ** child_ids; static size_t n_children, cap_children;

static void spawn(char ** fields, int count) {
  fflush(NULL);
  pid_t parent = getpid();
  pid_t pid = fork();
  if (pid == 0) {
#ifdef __linux__
    /* a child never outlives the zygote, so flume can fall back to `lean` safely */
    prctl(PR_SET_PDEATHSIG, SIGKILL);
    if (getppid() != parent) _exit(127);
#endif
    signal(SIGCHLD, SIG_DFL);
    close(sig_pipe[0]); close(sig_pipe[1]);
    int devnull = open("/dev/null", O_RDONLY);
    int out = open(fields[1], O_WRONLY | O_CREAT | O_TRUNC, 0644);
    int err = open(fields[2], O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (devnull < 0 || out < 0 || err < 0) _exit(127);
    dup2(devnull, 0); dup2(out, 1); dup2(err, 2);
    close(devnull); close(out); close(err);
    setenv("LEAN_PATH", fields[3], 1);
    int rc = child_main(count - 4, fields + 4);
    fflush(NULL);
    _exit(rc);
  }
  if (pid < 0) {
    printf("%s 127\n", fields[0]);
    fflush(stdout);
    return;
  }
  if (n_children == cap_children) {
    cap_children = cap_children ? 2 * cap_children : 32;
    child_pids = realloc(child_pids, cap_children * sizeof(pid_t));
    child_ids = realloc(child_ids, cap_children * sizeof(char *));
  }
  child_pids[n_children] = pid;
  child_ids[n_children] = strdup(fields[0]);
  n_children++;
}

static void reap(int block) {
  for (;;) {
    int st;
    pid_t pid = waitpid(-1, &st, block ? 0 : WNOHANG);
    if (pid <= 0) {
      if (pid < 0 && errno == EINTR) continue;
      return;
    }
    int code = WIFEXITED(st) ? WEXITSTATUS(st) : (WIFSIGNALED(st) ? 128 + WTERMSIG(st) : 127);
    for (size_t i = 0; i < n_children; i++) {
      if (child_pids[i] == pid) {
        printf("%s %d\n", child_ids[i], code);
        fflush(stdout);
        free(child_ids[i]);
        child_pids[i] = child_pids[n_children - 1];
        child_ids[i] = child_ids[n_children - 1];
        n_children--;
        break;
      }
    }
    if (block && n_children == 0) return;
  }
}

/* Input buffer of requests not yet parsed. */
static char * in_buf; static size_t in_len, in_cap;

/*
Parses one complete request at the start of `in_buf`. Returns the number of bytes it
used (0 if the request is incomplete), and its fields (`*count` is 0 for a stop request).
*/
static size_t parse_request(char *** fields_out, int * count) {
  char * end = memchr(in_buf, 0, in_len);
  if (!end) return 0;
  int n = atoi(in_buf);
  size_t pos = (size_t)(end - in_buf) + 1;
  if (n <= 0) { *count = 0; return pos; }
  char ** fields = calloc((size_t)n + 1, sizeof(char *));
  for (int i = 0; i < n; i++) {
    char * e = memchr(in_buf + pos, 0, in_len - pos);
    if (!e) { free(fields); return 0; }
    fields[i] = in_buf + pos;
    pos = (size_t)(e - in_buf) + 1;
  }
  for (int i = 0; i < n; i++) fields[i] = strdup(fields[i]);
  *fields_out = fields; *count = n;
  return pos;
}

int main(void) {
  lean_initialize();
  lean_set_panic_messages(true);
  if (pipe(sig_pipe) < 0) return 1;
  fcntl(sig_pipe[0], F_SETFL, O_NONBLOCK); fcntl(sig_pipe[1], F_SETFL, O_NONBLOCK);
  fcntl(sig_pipe[0], F_SETFD, FD_CLOEXEC); fcntl(sig_pipe[1], F_SETFD, FD_CLOEXEC);
  struct sigaction sa;
  memset(&sa, 0, sizeof sa);
  sa.sa_handler = on_sigchld;
  sa.sa_flags = SA_RESTART | SA_NOCLDSTOP;
  sigaction(SIGCHLD, &sa, NULL);
  /* flume sends requests only after this line, so a build that compiles a single module
     never waits for the initialization above */
  printf("ready\n");
  fflush(stdout);
  int accepting = 1;
  while (accepting) {
    struct pollfd fds[2] = {{0, POLLIN, 0}, {sig_pipe[0], POLLIN, 0}};
    if (poll(fds, 2, -1) < 0) { if (errno == EINTR) continue; break; }
    if (fds[1].revents) {
      char drain[64];
      while (read(sig_pipe[0], drain, sizeof drain) > 0) {}
      reap(0);
    }
    if (fds[0].revents) {
      if (in_len + 65536 > in_cap) { in_cap = in_len + 65536 * 2; in_buf = realloc(in_buf, in_cap); }
      ssize_t n = read(0, in_buf + in_len, in_cap - in_len);
      if (n <= 0) { if (n < 0 && errno == EINTR) continue; break; }
      in_len += (size_t)n;
      for (;;) {
        char ** fields; int count;
        size_t used = parse_request(&fields, &count);
        if (used == 0) break;
        memmove(in_buf, in_buf + used, in_len - used);
        in_len -= used;
        if (count == 0) { accepting = 0; break; }
        if (count >= 5) spawn(fields, count);
        for (int i = 0; i < count; i++) free(fields[i]);
        free(fields);
      }
    }
  }
  if (n_children > 0) reap(1);
  return 0;
}
