BINARY  := capacity_bench
CONFIG  ?= configs/graphql-catalog.yaml

.PHONY: build test vet run validate clean

build:
	go build -o $(BINARY) .

test:
	go test ./...

vet:
	go vet ./...

validate: build
	./$(BINARY) -config $(CONFIG) -validate

run: build
	./$(BINARY) -config $(CONFIG)

clean:
	rm -f $(BINARY)
	rm -rf reports
