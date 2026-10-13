#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* Hold only the selected image's metadata; the rest of the TUI stays real. */
static void gate(const char *path) {
    const char *wanted = getenv("THURBOX_TEST_METADATA_PATH");
    if (!wanted || !path || strcmp(wanted, path)) return;
    const char *marker = getenv("THURBOX_TEST_METADATA_ENTERED");
    const char *release = getenv("THURBOX_TEST_METADATA_RELEASE");
    if (!marker || !release) return;
    int fd = open(marker, O_CREAT | O_WRONLY, 0600);
    if (fd >= 0) close(fd);
    while (access(release, F_OK) != 0) usleep(10000);
}

int statx(int fd, const char *path, int flags, unsigned mask, struct statx *buf) {
    typedef int (*Statx)(int, const char *, int, unsigned, struct statx *);
    gate(path);
    return ((Statx)dlsym(RTLD_NEXT, "statx"))(fd, path, flags, mask, buf);
}

int stat(const char *path, struct stat *buf) {
    typedef int (*Stat)(const char *, struct stat *);
    gate(path);
    return ((Stat)dlsym(RTLD_NEXT, "stat"))(path, buf);
}
