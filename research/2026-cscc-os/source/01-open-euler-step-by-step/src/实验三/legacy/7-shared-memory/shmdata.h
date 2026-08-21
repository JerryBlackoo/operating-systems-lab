#ifndef _SHMDATA_H
#define _SHMDATA_H

#define TEXT_SIZE 2048

struct shared_data {
    int written;
    char text[TEXT_SIZE];
};

#endif
