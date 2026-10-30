/* A stand-in for deepdrivecleaner in the fake releases of test_install.py: built with
 * cc -DVERSION='"<v>"' and signed ad hoc as com.smartandpoint.deepdrivecleaner.cli.
 *   --version     prints "deepdrivecleaner <v>", as the real CLI does
 *   wait          waits for a line on stdin, then prints "deepdrivecleaner <v> waited":
 *                 a copy that runs while the script replaces it
 *   anything else prints "deepdrivecleaner <v> ran" (the LaunchAgent's scan --background)
 */
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "--version") == 0) {
        printf("deepdrivecleaner %s\n", VERSION);
    } else if (argc > 1 && strcmp(argv[1], "wait") == 0) {
        int c;
        while ((c = getchar()) != EOF && c != '\n') {
        }
        printf("deepdrivecleaner %s waited\n", VERSION);
    } else {
        printf("deepdrivecleaner %s ran\n", VERSION);
    }
    return 0;
}
