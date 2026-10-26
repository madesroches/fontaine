CC = cc
CFLAGS = -O2

all: app

app: main.o util.o
	$(CC) $(CFLAGS) -o app main.o util.o

clean:
	rm -f app *.o
