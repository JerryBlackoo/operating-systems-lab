#ifndef _MSG_DATA_H
#define _MSG_DATA_H

#define MAX_MSG_SIZE 512

struct my_msg {
    long msg_type;
    char msg_text[MAX_MSG_SIZE];
};

#endif
//
