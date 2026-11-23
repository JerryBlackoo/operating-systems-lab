#include "echo.h"
#include <stddef.h>

bool echo_on_reply(struct echo_app *app, int64_t status, const struct abi_message *reply)
{
    if (app->state != ECHO_WAITING)
        return false;

    bool matches = status == ABI_OK && reply && reply->tag == ECHO_REPLY &&
                   reply->reply == 0 && reply->words[0] == app->value;
    app->state = matches ? ECHO_DONE : ECHO_FAILED;
    app->port->report(app->port->ctx, app->state);
    app->port->release(app->port->ctx, app->endpoint);
    app->endpoint = 0;
    return true;
}

bool echo_start(struct echo_app *app)
{
    if (app->state != ECHO_READY || !app->endpoint)
        return false;
    const struct abi_message request = {.tag = ECHO_REQUEST, .words = {app->value}};
    app->state = ECHO_WAITING;
    int64_t status = app->port->call(app->port->ctx, app->endpoint, &request);
    if (status != ABI_OK)
        echo_on_reply(app, status, NULL);
    return true;
}
